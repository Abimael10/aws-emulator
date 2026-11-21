// SPDX-License-Identifier: CC0-1.0

//! The group-metric catalog `DescribeMetricCollectionTypes` publishes, over the
//! awsQuery wire.
//!
//! THE RULE UNDER TEST: a reference-data describe publishes the service's whole
//! current set of valid values, which is what a client enumerates to decide what
//! it may send — so where the on-disk model's prose for the member enumerates
//! fewer values than the service accepts, that prose is a lower bound and the
//! published catalog is the complete set, with nothing in it the service does not
//! accept.
//!
//! The table below is the class, not one row of it: it spans every metric the
//! catalog publishes — the sizes, the instance counts, the capacities, the
//! retained-capacity series and the warm pool, each row recording whether the
//! model member's own documentation enumerates it — and a second table of
//! near-miss names that are NOT metrics. The catalog itself is hand-written data
//! in `src/state.rs`, whose doc comment names its source: the model member
//! `MetricCollectionType$Metric` (read with
//! `smithy_query(query="shape",shape="com.amazonaws.autoscaling#MetricCollectionType")`)
//! for the metrics it enumerates, plus the warm pool minimum and the
//! retained-capacity series that member's documentation omits.
//!
//! Every assertion drives the service the way the router does — one
//! `MultiServiceRouter` holding this service over one shared engine — so what is
//! under test is the HTTP behaviour, not a handler called directly. Stability
//! across reads and across what the store holds belongs to
//! `reference_data_catalogs.rs`, which walks every catalog through it.
//!
//! # The metrics collection a group turns on and off
//!
//! THE SECOND RULE UNDER TEST, further down this file: the group metrics a group
//! aggregates to CloudWatch are STATE OF THAT GROUP — the metrics an enable names,
//! or every metric of the published catalog when it names none, each at the
//! granularity the enable asked for, reported by every later read of the group
//! until a disable takes them back out, and refused with a 400 `ValidationError`
//! that writes nothing when the request names a metric or a granularity the
//! catalog does not publish.
//!
//! Those tables span what that rule generalises over: how many metrics a request
//! names (none, one, several, the same one twice, the whole published catalog),
//! which ones, the order it names them in, whether they are already collected, and
//! the kind of bad value — each row reading the catalog, and the granularity, off
//! the wire rather than writing them out.

use crate::support::{
    borrowed, new_engine, p, query, router_with, seed_subnets, tag_values, xml_unescape,
};

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use autoscaling::state;

/// One metric the catalog must publish.
struct MetricCase {
    /// The metric's name, as `EnableMetricsCollection` names it.
    metric: &'static str,
    /// What the metric reports on, so a row names its part of the class.
    family: &'static str,
    /// Whether the model member's own documentation enumerates this metric.
    /// `false` marks a metric the service publishes and that documentation is
    /// behind on.
    in_model_prose: bool,
}

/// Every group metric the service accepts, one row each.
const METRICS: &[MetricCase] = &[
    MetricCase { metric: "GroupMinSize", family: "group size", in_model_prose: true },
    MetricCase { metric: "GroupMaxSize", family: "group size", in_model_prose: true },
    MetricCase { metric: "GroupDesiredCapacity", family: "group size", in_model_prose: true },
    MetricCase { metric: "GroupInServiceInstances", family: "group instances", in_model_prose: true },
    MetricCase { metric: "GroupPendingInstances", family: "group instances", in_model_prose: true },
    MetricCase { metric: "GroupStandbyInstances", family: "group instances", in_model_prose: true },
    MetricCase { metric: "GroupTerminatingInstances", family: "group instances", in_model_prose: true },
    MetricCase {
        metric: "GroupTerminatingRetainedInstances",
        family: "group instances (retained)",
        in_model_prose: false,
    },
    MetricCase { metric: "GroupTotalInstances", family: "group instances", in_model_prose: true },
    MetricCase { metric: "GroupInServiceCapacity", family: "group capacity", in_model_prose: true },
    MetricCase { metric: "GroupPendingCapacity", family: "group capacity", in_model_prose: true },
    MetricCase { metric: "GroupStandbyCapacity", family: "group capacity", in_model_prose: true },
    MetricCase { metric: "GroupTerminatingCapacity", family: "group capacity", in_model_prose: true },
    MetricCase {
        metric: "GroupTerminatingRetainedCapacity",
        family: "group capacity (retained)",
        in_model_prose: false,
    },
    MetricCase { metric: "GroupTotalCapacity", family: "group capacity", in_model_prose: true },
    MetricCase { metric: "WarmPoolMinSize", family: "warm pool size", in_model_prose: false },
    MetricCase { metric: "WarmPoolDesiredCapacity", family: "warm pool capacity", in_model_prose: true },
    MetricCase { metric: "WarmPoolWarmedCapacity", family: "warm pool capacity", in_model_prose: true },
    MetricCase { metric: "WarmPoolPendingCapacity", family: "warm pool capacity", in_model_prose: true },
    MetricCase {
        metric: "WarmPoolPendingRetainedCapacity",
        family: "warm pool capacity (retained)",
        in_model_prose: false,
    },
    MetricCase {
        metric: "WarmPoolTerminatingCapacity",
        family: "warm pool capacity",
        in_model_prose: true,
    },
    MetricCase {
        metric: "WarmPoolTerminatingRetainedCapacity",
        family: "warm pool capacity (retained)",
        in_model_prose: false,
    },
    MetricCase { metric: "WarmPoolTotalCapacity", family: "warm pool capacity", in_model_prose: true },
    MetricCase {
        metric: "GroupAndWarmPoolDesiredCapacity",
        family: "group and warm pool",
        in_model_prose: true,
    },
    MetricCase {
        metric: "GroupAndWarmPoolTotalCapacity",
        family: "group and warm pool",
        in_model_prose: true,
    },
];

/// Names a client might plausibly send that are NOT group metrics: the catalog
/// publishes the complete set and nothing beyond it, so each of these must be
/// absent from the answer and rejected by the accepted-metric check.
///
/// One per way of being wrong: a family that has no such series, a bound that
/// exists for the group but not for the warm pool, a case difference, a singular
/// where the catalog is plural, a retained form of a metric that has none, and a
/// name padded with whitespace.
const NOT_METRICS: &[&str] = &[
    "GroupRetainedInstances",
    "WarmPoolMaxSize",
    "groupminsize",
    "GroupTerminatingRetainedInstance",
    "GroupMinSizeRetained",
    " WarmPoolMinSize",
];

/// The whole catalog, read off the wire: every metric of [`METRICS`] published
/// exactly once, in the order the service holds it, with no entry the service
/// does not accept.
#[tokio::test]
async fn the_published_catalog_is_every_group_metric_the_service_accepts() {
    let router = router_with(new_engine());

    let (status, body) = query(&router, "DescribeMetricCollectionTypes", &[]).await;
    assert_eq!(
        status, 200,
        "DescribeMetricCollectionTypes: a Unit-input read models no 4xx, got {status}: {body}"
    );
    let published = tag_values(&body, "Metric");

    for case in METRICS {
        let occurrences = published.iter().filter(|entry| *entry == case.metric).count();
        assert_eq!(
            occurrences, 1,
            "case {:?} ({}, in model prose: {}): expected the catalog to publish it exactly \
             once, got {occurrences} occurrences in {published:?}",
            case.metric, case.family, case.in_model_prose
        );
    }

    // The rows the model member's documentation omits are published all the same:
    // the prose is a lower bound, the catalog is the set.
    let omitted_by_prose: Vec<&str> = METRICS
        .iter()
        .filter(|case| !case.in_model_prose)
        .map(|case| case.metric)
        .collect();
    assert!(
        !omitted_by_prose.is_empty(),
        "the table must still span the metrics the model member's prose omits, \
         got {omitted_by_prose:?}"
    );
    for metric in &omitted_by_prose {
        assert!(
            published.iter().any(|entry| entry == metric),
            "case {metric:?}: a metric the service accepts must be published even where the \
             model member's documentation omits it, got {published:?}"
        );
    }

    for metric in NOT_METRICS {
        assert!(
            !published.iter().any(|entry| entry == metric),
            "case {metric:?}: the catalog publishes only metrics the service accepts, \
             got {published:?}"
        );
    }

    // No entry beyond the table: the published set and the table are the same set.
    for entry in &published {
        assert!(
            METRICS.iter().any(|case| case.metric == entry),
            "case {entry:?}: published but not a group metric this suite knows, \
             got {published:?}"
        );
    }
    assert_eq!(
        published.len(),
        METRICS.len(),
        "expected the catalog to publish {} metrics, got {published:?}",
        METRICS.len()
    );

    // The second published list: the one granularity metrics collection accepts.
    let granularities = tag_values(&body, "Granularity");
    assert_eq!(
        granularities,
        vec!["1Minute".to_string()],
        "expected the single published granularity, got {granularities:?}"
    );
}

/// The published catalog and the metrics the mutating operations accept are ONE
/// list: the accepted-metric check answers true for every name the catalog
/// publishes and false for every near miss, so neither side can drift from the
/// other.
#[tokio::test]
async fn the_accepted_metric_check_is_the_published_catalog() {
    let router = router_with(new_engine());

    let (status, body) = query(&router, "DescribeMetricCollectionTypes", &[]).await;
    assert_eq!(status, 200, "DescribeMetricCollectionTypes: got {status}: {body}");
    let published = tag_values(&body, "Metric");

    // The positive side is driven from this suite's own table, not from the
    // answer: a name the predicate and the response both read off one constant
    // would agree however wrong that constant was.
    for case in METRICS {
        assert!(
            state::is_valid_metric_collection_type(case.metric),
            "case {:?} ({}): a group metric must be accepted, got false",
            case.metric,
            case.family
        );
        assert!(
            published.iter().any(|entry| entry == case.metric),
            "case {:?} ({}): an accepted metric must be published, got {published:?}",
            case.metric,
            case.family
        );
    }
    for metric in NOT_METRICS {
        assert!(
            !state::is_valid_metric_collection_type(metric),
            "case {metric:?}: a name the catalog does not publish must not be accepted, got true"
        );
    }
}

// Stability across repeated reads and across what the store holds is NOT asserted
// again here: `reference_data_catalogs.rs` already reads every catalog, this one
// among them, before and after the store gains and loses a group.

// ---------------------------------------------------------------------------
// The metrics collection ONE group turns on and off.
// ---------------------------------------------------------------------------

/// The launch source every group below is created from: a launch TEMPLATE, because no
/// account can create a launch configuration (`CreateLaunchConfiguration` is refused).
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// The metrics and the granularities the service publishes, read off the wire, so no row
/// below writes either of them out.
async fn published(
    router: &smithy_server_runtime::MultiServiceRouter,
) -> (Vec<String>, Vec<String>) {
    let (status, body) = query(router, "DescribeMetricCollectionTypes", &[]).await;
    assert_eq!(
        status, 200,
        "the published catalog: expected 200, got {status}: {body}"
    );
    let metrics = tag_values(&body, "Metric");
    let granularities = tag_values(&body, "Granularity");
    assert!(
        metrics.len() > 1,
        "the published catalog must name more than one metric, got {metrics:?}"
    );
    assert!(
        !granularities.is_empty(),
        "the published catalog must name a granularity, got {granularities:?}"
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
            LAUNCH_SOURCE,
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

/// `EnableMetricsCollection` for one group, naming the metrics `metrics` lists (naming none
/// when it is empty) at `granularity`.
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

/// `DisableMetricsCollection` for one group, naming the metrics `metrics` lists (naming
/// none when it is empty).
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

/// The metrics collection one group reports, as `(Metric, Granularity)` pairs in the order
/// `DescribeAutoScalingGroups` reports them.
async fn reported_metrics(
    router: &smithy_server_runtime::MultiServiceRouter,
    group: &str,
) -> Vec<(String, String)> {
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
    let metrics = tag_values(&body, "Metric");
    let granularities = tag_values(&body, "Granularity");
    assert_eq!(
        metrics.len(),
        granularities.len(),
        "every collected metric reports a metric and a granularity, got {metrics:?} and \
         {granularities:?} from {body}"
    );
    metrics.into_iter().zip(granularities).collect()
}

/// The metrics of `wanted`, in the order the published catalog holds them — which is the
/// order a group reports them in, whatever order the requests named them in.
fn in_catalog_order(catalog: &[String], wanted: &[&str]) -> Vec<String> {
    let ordered: Vec<String> = catalog
        .iter()
        .filter(|metric| wanted.contains(&metric.as_str()))
        .cloned()
        .collect();
    assert_eq!(
        ordered.len(),
        {
            let mut unique: Vec<&str> = wanted.to_vec();
            unique.sort_unstable();
            unique.dedup();
            unique.len()
        },
        "every metric a case names must be one the catalog publishes, got {ordered:?} for \
         {wanted:?}"
    );
    ordered
}

/// Every metric of the published catalog except the ones `removed` names.
fn every_published_except(catalog: &[String], removed: &[&str]) -> Vec<String> {
    catalog
        .iter()
        .filter(|metric| !removed.contains(&metric.as_str()))
        .cloned()
        .collect()
}

/// One row of the enable table.
struct EnableCase {
    case: &'static str,
    /// The metrics the request names; empty names none at all.
    named: &'static [&'static str],
    /// `true` for a row that names the whole published catalog instead of [`Self::named`].
    whole_catalog: bool,
    /// The metrics the group then reports, or `None` for every published metric.
    expected: Option<&'static [&'static str]>,
}

/// An enable records the metrics the request names — or every metric of the published
/// catalog when it names none — against the group it names, each at the granularity it
/// asked for, and they are read back through `DescribeAutoScalingGroups`.
#[tokio::test]
async fn enabling_metrics_collection_records_the_metrics_the_request_names() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);
    let (catalog, granularities) = published(&router).await;
    let granularity = granularities[0].clone();

    let cases = [
        EnableCase {
            case: "no metric named: every published metric is collected",
            named: &[],
            whole_catalog: false,
            expected: None,
        },
        EnableCase {
            case: "the whole published catalog named explicitly",
            named: &[],
            whole_catalog: true,
            expected: None,
        },
        EnableCase {
            case: "one metric",
            named: &["GroupMinSize"],
            whole_catalog: false,
            expected: Some(&["GroupMinSize"]),
        },
        EnableCase {
            case: "three metrics, named out of the order they are reported in",
            named: &["WarmPoolTotalCapacity", "GroupTotalCapacity", "GroupMinSize"],
            whole_catalog: false,
            expected: Some(&["GroupMinSize", "GroupTotalCapacity", "WarmPoolTotalCapacity"]),
        },
        EnableCase {
            case: "the same metric twice: one entry",
            named: &["GroupDesiredCapacity", "GroupDesiredCapacity"],
            whole_catalog: false,
            expected: Some(&["GroupDesiredCapacity"]),
        },
        EnableCase {
            // A metric the model member's own prose omits and the catalog publishes all
            // the same, so the enable is driven by the catalog rather than by that prose.
            case: "a metric the model member's documentation omits",
            named: &["WarmPoolMinSize"],
            whole_catalog: false,
            expected: Some(&["WarmPoolMinSize"]),
        },
        EnableCase {
            case: "two retained-capacity metrics of different families",
            named: &[
                "WarmPoolTerminatingRetainedCapacity",
                "GroupTerminatingRetainedCapacity",
            ],
            whole_catalog: false,
            expected: Some(&[
                "GroupTerminatingRetainedCapacity",
                "WarmPoolTerminatingRetainedCapacity",
            ]),
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let group = format!("metrics-{index}");
        create_group(&router, &group, &subnets[0].id).await;
        let before = reported_metrics(&router, &group).await;
        assert!(
            before.is_empty(),
            "case {:?}: a new group collects no group metric, got {before:?}",
            case.case
        );

        let named: Vec<String> = if case.whole_catalog {
            catalog.clone()
        } else {
            case.named.iter().map(|m| (*m).to_string()).collect()
        };
        let (status, body) = enable(&router, &group, &named, &granularity).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200, got {status}: {body}",
            case.case
        );
        assert!(
            body.contains("<EnableMetricsCollectionResponse"),
            "case {:?}: expected the operation's own response envelope, got {body}",
            case.case
        );
        assert!(
            !body.contains("<ErrorResponse"),
            "case {:?}: a served enable carries no error envelope, got {body}",
            case.case
        );

        let expected = case
            .expected
            .map_or_else(|| catalog.clone(), |wanted| in_catalog_order(&catalog, wanted));
        let reported = reported_metrics(&router, &group).await;
        assert_eq!(
            reported
                .iter()
                .map(|(metric, _)| metric.clone())
                .collect::<Vec<String>>(),
            expected,
            "case {:?}: expected the collected metrics {expected:?}, got {reported:?}",
            case.case
        );
        for (metric, reported_granularity) in &reported {
            assert_eq!(
                reported_granularity, &granularity,
                "case {:?}: {metric} must be reported at the granularity the enable asked \
                 for, got {reported_granularity:?}",
                case.case
            );
        }
        // The same collection on a second read: a read reports state, it does not move it.
        let again = reported_metrics(&router, &group).await;
        assert_eq!(
            again, reported,
            "case {:?}: a second read reports the same collection, got {again:?} after \
             {reported:?}",
            case.case
        );
    }
}

/// What a group collects after a disable.
enum Remaining {
    /// Every published metric but the ones named here.
    EveryPublishedExcept(&'static [&'static str]),
    /// Exactly the metrics named here, in the catalog's own order.
    Exactly(&'static [&'static str]),
}

/// One row of the disable table.
struct DisableCase {
    case: &'static str,
    /// The metrics enabled first; empty names none at all, which collects every one.
    enabled: &'static [&'static str],
    /// The metrics the disable names; empty names none at all.
    disabled: &'static [&'static str],
    /// `true` for a row whose disable names the whole published catalog instead.
    disable_whole_catalog: bool,
    /// What the group collects afterwards.
    remaining: Remaining,
}

/// A disable takes out the metrics the request names — or every metric of the published
/// catalog when it names none — and leaves every other metric of the group collected.
#[tokio::test]
async fn disabling_metrics_collection_takes_out_only_the_metrics_the_request_names() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);
    let (catalog, granularities) = published(&router).await;
    let granularity = granularities[0].clone();

    let cases = [
        DisableCase {
            case: "every metric collected, one disabled: the rest stay collected",
            enabled: &[],
            disabled: &["GroupMinSize"],
            disable_whole_catalog: false,
            remaining: Remaining::EveryPublishedExcept(&["GroupMinSize"]),
        },
        DisableCase {
            case: "one of three collected metrics disabled",
            enabled: &["GroupMinSize", "GroupMaxSize", "WarmPoolMinSize"],
            disabled: &["GroupMaxSize"],
            disable_whole_catalog: false,
            remaining: Remaining::Exactly(&["GroupMinSize", "WarmPoolMinSize"]),
        },
        DisableCase {
            case: "no metric named: everything the group collects is taken out",
            enabled: &["GroupDesiredCapacity"],
            disabled: &[],
            disable_whole_catalog: false,
            remaining: Remaining::Exactly(&[]),
        },
        DisableCase {
            case: "a metric the group does not collect: nothing changes",
            enabled: &["GroupDesiredCapacity"],
            disabled: &["GroupTotalCapacity"],
            disable_whole_catalog: false,
            remaining: Remaining::Exactly(&["GroupDesiredCapacity"]),
        },
        DisableCase {
            case: "the whole published catalog named explicitly",
            enabled: &[],
            disabled: &[],
            disable_whole_catalog: true,
            remaining: Remaining::Exactly(&[]),
        },
        DisableCase {
            case: "the same metric twice: it is taken out once",
            enabled: &["GroupTerminatingRetainedInstances", "GroupTotalInstances"],
            disabled: &[
                "GroupTerminatingRetainedInstances",
                "GroupTerminatingRetainedInstances",
            ],
            disable_whole_catalog: false,
            remaining: Remaining::Exactly(&["GroupTotalInstances"]),
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let group = format!("disable-{index}");
        create_group(&router, &group, &subnets[0].id).await;

        let enabled: Vec<String> = case.enabled.iter().map(|m| (*m).to_string()).collect();
        let (status, body) = enable(&router, &group, &enabled, &granularity).await;
        assert_eq!(
            status, 200,
            "case {:?}: the enable expected 200, got {status}: {body}",
            case.case
        );

        let disabled: Vec<String> = if case.disable_whole_catalog {
            catalog.clone()
        } else {
            case.disabled.iter().map(|m| (*m).to_string()).collect()
        };
        let (status, body) = disable(&router, &group, &disabled).await;
        assert_eq!(
            status, 200,
            "case {:?}: the disable expected 200, got {status}: {body}",
            case.case
        );
        assert!(
            body.contains("<DisableMetricsCollectionResponse"),
            "case {:?}: expected the operation's own response envelope, got {body}",
            case.case
        );

        let expected = match case.remaining {
            Remaining::EveryPublishedExcept(removed) => every_published_except(&catalog, removed),
            Remaining::Exactly(wanted) => in_catalog_order(&catalog, wanted),
        };
        let reported = reported_metrics(&router, &group).await;
        assert_eq!(
            reported
                .iter()
                .map(|(metric, _)| metric.clone())
                .collect::<Vec<String>>(),
            expected,
            "case {:?}: expected the collected metrics {expected:?}, got {reported:?}",
            case.case
        );
    }
}

/// Which operation a refusal row drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggle {
    Enable,
    Disable,
}

/// One row of the granularity table: what the enable asks to aggregate at.
struct GranularityCase {
    case: &'static str,
    granularity: &'static str,
}

/// A request naming a metric the catalog does not publish, a granularity it does not
/// publish, or a group the account does not hold is refused with a 400 `ValidationError`
/// and leaves the group's metrics collection as it was.
#[tokio::test]
async fn a_metrics_request_outside_the_published_catalog_is_refused_and_writes_nothing() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);
    let (catalog, granularities) = published(&router).await;
    let granularity = granularities[0].clone();

    create_group(&router, "held", &subnets[0].id).await;
    let held = ["GroupMinSize".to_string()];
    let (status, body) = enable(&router, "held", &held, &granularity).await;
    assert_eq!(status, 200, "the held group's enable: got {status}: {body}");
    let untouched = reported_metrics(&router, "held").await;
    assert_eq!(
        untouched.len(),
        1,
        "the held group collects the one metric it was given, got {untouched:?}"
    );

    // The metrics the caller may choose from, as the service publishes them.
    let valid_metrics = format!("Valid metrics are: [{}].", catalog.join(", "));
    let valid_granularities = format!(
        "Valid metric granularity values are: [{}].",
        granularities.join(", ")
    );

    // Every near-miss name of this suite's own table, through BOTH writers of a group's
    // metrics collection: neither may write a metric the catalog does not publish.
    for metric in NOT_METRICS {
        for toggle in [Toggle::Enable, Toggle::Disable] {
            let named = [(*metric).to_string()];
            let (status, body) = match toggle {
                Toggle::Enable => enable(&router, "held", &named, &granularity).await,
                Toggle::Disable => disable(&router, "held", &named).await,
            };
            let case = format!("{toggle:?} {metric:?}");
            assert_eq!(status, 400, "case {case}: expected 400, got {status}: {body}");
            assert!(
                body.contains("<Code>ValidationError</Code>"),
                "case {case}: expected the ValidationError code, got {body}"
            );
            assert!(
                xml_unescape(&body).contains(&valid_metrics),
                "case {case}: expected the message {valid_metrics:?}, got {body}"
            );
            let after = reported_metrics(&router, "held").await;
            assert_eq!(
                after, untouched,
                "case {case}: a refused request leaves the group's collection as it was, \
                 got {after:?} after {untouched:?}"
            );
        }
    }

    // One per way of naming a granularity the catalog does not publish: another period,
    // the published one in another case, and the published one padded or split.
    let granularity_cases = [
        GranularityCase {
            case: "a period the service does not aggregate at",
            granularity: "5Minute",
        },
        GranularityCase {
            case: "the published granularity in another case",
            granularity: "1minute",
        },
        GranularityCase {
            case: "the published granularity with surrounding space",
            granularity: " 1Minute",
        },
        GranularityCase {
            case: "the published granularity split by a space",
            granularity: "1 Minute",
        },
        GranularityCase {
            case: "a period stated in another unit",
            granularity: "60Seconds",
        },
    ];
    for case in &granularity_cases {
        let named = ["GroupMaxSize".to_string()];
        let (status, body) = enable(&router, "held", &named, case.granularity).await;
        assert_eq!(
            status, 400,
            "case {:?} ({:?}): expected 400, got {status}: {body}",
            case.case, case.granularity
        );
        assert!(
            body.contains("<Code>ValidationError</Code>"),
            "case {:?}: expected the ValidationError code, got {body}",
            case.case
        );
        assert!(
            xml_unescape(&body).contains(&valid_granularities),
            "case {:?}: expected the message {valid_granularities:?}, got {body}",
            case.case
        );
        let after = reported_metrics(&router, "held").await;
        assert_eq!(
            after, untouched,
            "case {:?}: a refused enable leaves the group's collection as it was, got \
             {after:?} after {untouched:?}",
            case.case
        );
    }

    // A group the account does not hold, through both writers: the sentence these two
    // operations state for one — one sentence about the group, naming it.
    for toggle in [Toggle::Enable, Toggle::Disable] {
        let named = ["GroupMinSize".to_string()];
        let (status, body) = match toggle {
            Toggle::Enable => enable(&router, "no-such-group", &named, &granularity).await,
            Toggle::Disable => disable(&router, "no-such-group", &named).await,
        };
        assert_eq!(
            status, 400,
            "case {toggle:?} on a group the account does not hold: expected 400, got \
             {status}: {body}"
        );
        assert!(
            xml_unescape(&body).contains("Group no-such-group not found"),
            "case {toggle:?} on a group the account does not hold: expected the \
             absent-group refusal, got {body}"
        );
    }

    // And the granularity the catalog does publish is served, on the same group the
    // refusals above left alone: the refusals are about the value, not about the group.
    let named = ["GroupMaxSize".to_string()];
    let (status, body) = enable(&router, "held", &named, &granularity).await;
    assert_eq!(
        status, 200,
        "the published granularity must be served, got {status}: {body}"
    );
    let after = reported_metrics(&router, "held").await;
    assert_eq!(
        after.len(),
        2,
        "the served enable adds the metric it names to the one the group held, got \
         {after:?}"
    );
}
