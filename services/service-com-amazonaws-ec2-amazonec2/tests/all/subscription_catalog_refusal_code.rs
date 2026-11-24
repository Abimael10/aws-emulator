// SPDX-License-Identifier: CC0-1.0

//! How the INFRASTRUCTURE PERFORMANCE subscription surface refuses a value outside one of its
//! catalogs, driven over the wire the service actually speaks — `ec2Query`, through the
//! crate's shared [`super::ec2_query`] harness.
//!
//! The rule under test: the metric a subscription measures and the statistic it reports are
//! modelled enums with one member each (`MetricType` is `aggregate-latency` and
//! `StatisticType` is `p50`:
//! `smithy_query(query="shape",shape="MetricType,StatisticType")`), so any other value is a
//! bad parameter VALUE rather than an unknown feature. This surface validates those members
//! through EC2's request-parameter layer, which answers its own `InvalidParameter` with the
//! sentence that names the member as the API spells it, the value the request sent and the
//! values the member takes — not the framework's model-constraint code, which is no EC2 error
//! code and which no client's EC2 error handling matches. Nothing is subscribed either way.
//!
//! The rows differ along which member carries the value, which operation of the surface
//! received it, and what kind of value it is — the other enum's member, a different spelling
//! of the published one, and a plausible value neither enum publishes.

use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, INVALID_PARAMETER};
use service_com_amazonaws_ec2_amazonec2 as ec2;

use super::ec2_query::{call, error_code, error_message};

/// The Region every call of this suite is signed for. A subscription is named by the Regions
/// the REQUEST names, so the signed one only decides which endpoint answers.
const REGION: &str = "us-east-1";

/// The one member `MetricType` publishes.
const METRIC: &str = "aggregate-latency";

/// The one member `StatisticType` publishes.
const STATISTIC: &str = "p50";

/// The two operations of the surface that name a subscription by those four members.
const OPERATIONS: [&str; 2] = [
    "EnableAwsNetworkPerformanceMetricSubscription",
    "DisableAwsNetworkPerformanceMetricSubscription",
];

fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// The form fields naming one subscription.
fn naming(metric: &str, statistic: &str) -> Vec<(String, String)> {
    vec![
        p("Source", "us-east-1"),
        p("Destination", "eu-west-1"),
        p("Metric", metric),
        p("Statistic", statistic),
    ]
}

/// The sentence EC2's request-parameter layer words for a value outside a published value
/// set: the member as the API spells it, the value the request sent, and the values it takes.
fn expected_message(member: &str, value: &str, allowed: &str) -> String {
    format!("Invalid value for {member}: {value} Valid values are: [{allowed}]")
}

/// A value outside either catalog is refused with EC2's parameter code and that layer's
/// sentence, by every operation of the surface, and nothing is subscribed.
#[tokio::test]
async fn a_value_outside_the_subscription_catalogs_is_refused_as_a_parameter() {
    // (case, the Metric, the Statistic, the member the sentence names, the value it names,
    // the value set it states)
    let rows: [(&str, &str, &str, &str, &str, &str); 6] = [
        (
            "a percentile the statistic enum does not publish",
            METRIC,
            "p99",
            "Statistic",
            "p99",
            STATISTIC,
        ),
        (
            "the published statistic in another case",
            METRIC,
            "P50",
            "Statistic",
            "P50",
            STATISTIC,
        ),
        (
            "the metric enum's member in the statistic's place",
            METRIC,
            METRIC,
            "Statistic",
            METRIC,
            STATISTIC,
        ),
        (
            "a metric the metric enum does not publish",
            "aggregate-throughput",
            STATISTIC,
            "Metric",
            "aggregate-throughput",
            METRIC,
        ),
        (
            "the published metric in another case",
            "Aggregate-Latency",
            STATISTIC,
            "Metric",
            "Aggregate-Latency",
            METRIC,
        ),
        (
            "the statistic enum's member in the metric's place",
            STATISTIC,
            STATISTIC,
            "Metric",
            STATISTIC,
            METRIC,
        ),
    ];

    for (case, metric, statistic, member, value, allowed) in rows {
        for action in OPERATIONS {
            let case = format!("{case} sent to {action}");
            let (router, engine) = super::ec2_query::emulator();
            let (status, body) = call(&router, REGION, action, &naming(metric, statistic)).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                INVALID_PARAMETER,
                "case {case:?}: expected the code {INVALID_PARAMETER:?}, got {body}"
            );
            assert_eq!(
                error_message(&body),
                expected_message(member, value, allowed),
                "case {case:?}: expected the parameter layer's sentence, got {body}"
            );
            assert!(
                engine
                    .state::<Ec2State>()
                    .lock()
                    .network_performance_subscriptions
                    .is_empty(),
                "case {case:?}: a refused request must subscribe nothing"
            );
        }
    }
}

/// The control: the members the model DOES publish are accepted, so the refusals above are
/// the value's doing rather than a surface this service never serves.
#[tokio::test]
async fn the_published_metric_and_statistic_are_accepted() {
    let (router, engine) = super::ec2_query::emulator();
    let case = "the published members";
    let (status, body) = call(
        &router,
        REGION,
        "EnableAwsNetworkPerformanceMetricSubscription",
        &naming(METRIC, STATISTIC),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );
    assert_eq!(
        engine
            .state::<Ec2State>()
            .lock()
            .network_performance_subscriptions
            .len(),
        1,
        "case {case:?}: the accepted request must subscribe exactly one pair"
    );
}
