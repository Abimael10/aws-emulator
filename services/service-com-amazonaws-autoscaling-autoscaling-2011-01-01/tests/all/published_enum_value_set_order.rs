// SPDX-License-Identifier: CC0-1.0

//! The value set a refusal of an enumerated member states, and the ORDER it lists it in.
//!
//! The rule under test: a request carrying a value outside an enumerated type is refused
//! with the value set the SERVICE publishes for that TYPE, listed in the one order it
//! lists that set in — the values read out of a hash table keyed on each value's own text
//! rather than out of the type's declaration. The set and its order belong to the type, so
//! neither depends on how the submitted value missed the set, on what else the request
//! carried, on another member of the same request being wrong, nor on the order the values
//! are presented to the derivation in. Everything around the set stays the framework's:
//! the envelope, the member path and the echoed value.
//!
//! The tables below therefore span what that rule generalises over: the forms a submitted
//! value can miss the set in, the members a request carries beside the offending one, a
//! request that breaks a second member's constraint as well, and the order the values reach
//! the derivation in. Each row asserts the WHOLE message, so a row cannot pass by refusing
//! for another reason, and the last test accepts every value the model declares, so a
//! refusal cannot pass by refusing everything.
//!
//! The set the service publishes is `client_error_refusal_wording`'s
//! `PUBLISHED_METRIC_TYPES`, this crate's one statement of it; this suite reads the values
//! back out of it rather than writing them again.

use service_common::validation_message::hashed_enum_order;
use smithy_server_runtime::MultiServiceRouter;

use crate::client_error_refusal_wording::{
    PREDEFINED_METRIC_TYPE_PARAMETER, PREDEFINED_METRIC_TYPE_PATH, PUBLISHED_METRIC_TYPES,
};
use crate::support::{borrowed, new_engine, p, query, router_with, tag_value, xml_unescape};

/// The values the model this emulator ships declares for the enumerated type a
/// target-tracking policy names its predefined metric from, in the order it declares them
/// (`smithy_query(query="shape",shape="MetricType")`).
const DECLARED_METRIC_TYPES: [&str; 4] = [
    "ASGAverageCPUUtilization",
    "ASGAverageNetworkIn",
    "ASGAverageNetworkOut",
    "ALBRequestCountPerTarget",
];

/// The one predefined metric type the SERVICE publishes that the shipped model does not
/// declare: the high-resolution CPU metric.
///
/// SOURCE: the service's own refusal of a value outside the set, which lists five values
/// where the model declares four.
const PUBLISHED_BEYOND_THE_MODEL: &str = "ASGAverageCPUUtilizationHighResolution";

/// The launch source every group here is created from — a launch TEMPLATE, because no
/// account can create a launch configuration (see
/// `launch_configurations_are_unavailable.rs`).
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// A resource label naming one Application Load Balancer target group, in the form that
/// member documents (`PredefinedMetricSpecification$ResourceLabel`): the final portion of
/// the load balancer ARN and of the target group ARN, separated by a forward slash.
const RESOURCE_LABEL: &str =
    "app/my-alb/778d41231b141a0f/targetgroup/my-alb-target-group/943f017f100becff";

/// Create one group with a launch template, in one Availability Zone, and assert the
/// create succeeded.
async fn create_group(router: &MultiServiceRouter, name: &str) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "0"),
        p("AvailabilityZones.member.1", "us-west-2a"),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating the group {name:?}: expected HTTP 200, got {status} with {body:?}"
    );
}

/// The parameters of a target-tracking put whose predefined metric type is `metric_type`,
/// carrying the members `extra` beside it.
fn target_tracking_put(
    group: &str,
    policy: &str,
    metric_type: &str,
    extra: &[(&str, &str)],
) -> Vec<(String, String)> {
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("PolicyName", policy),
        p("PolicyType", "TargetTrackingScaling"),
        p(PREDEFINED_METRIC_TYPE_PARAMETER, metric_type),
        p("TargetTrackingConfiguration.TargetValue", "50"),
    ];
    for (key, value) in extra {
        params.push(p(key, *value));
    }
    params
}

/// The error type, the wire code and the unescaped message of one answer.
fn refusal(body: &str) -> (String, String, String) {
    (
        tag_value(body, "Type"),
        tag_value(body, "Code"),
        xml_unescape(&tag_value(body, "Message")),
    )
}

/// The whole message a value outside the metric type's set is refused with.
fn expected_message(rejected: &str) -> String {
    format!(
        "1 validation error detected: Value '{rejected}' at '{PREDEFINED_METRIC_TYPE_PATH}' \
         failed to satisfy constraint: Member must satisfy enum value set: \
         [{PUBLISHED_METRIC_TYPES}]"
    )
}

/// The values a clause lists between its brackets, in the order it lists them.
fn stated_values(message: &str) -> Vec<&str> {
    let set = message
        .split_once("enum value set: [")
        .unwrap_or_else(|| panic!("the message must state a value set, got {message:?}"))
        .1
        .trim_end_matches(']');
    set.split(", ").collect()
}

/// Values a client can submit for the predefined metric type that the enumerated type
/// does not admit, differing in HOW they miss it: a name no metric type spells, a declared
/// value with a suffix, a declared value whose case differs, a declared value with
/// surrounding space, a metric type of another service's own enumeration, a value of
/// another enumerated type of this model, and text that spells no metric at all.
fn values_outside_the_set() -> Vec<(&'static str, &'static str)> {
    vec![
        ("a name no metric type spells", "ASGAverageDiskReads"),
        ("a declared value with a suffix", "ASGAverageNetworkInBytes"),
        ("a declared value in the wrong case", "AsgAverageNetworkOut"),
        (
            "a declared value with surrounding space",
            " ASGAverageCPUUtilization ",
        ),
        (
            "a metric type of another service",
            "ECSServiceAverageMemoryUtilization",
        ),
        (
            "a value of another enumerated type of this model",
            "Hibernated",
        ),
        ("text that spells no metric at all", "42"),
    ]
}

/// The members a target-tracking put can carry beside the offending one, which decide
/// nothing about the set a refusal states.
fn request_shapes() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        ("the members a target-tracking put needs", Vec::new()),
        (
            "a resource label and scale-in disabled as well",
            vec![
                (
                    "TargetTrackingConfiguration.PredefinedMetricSpecification.ResourceLabel",
                    RESOURCE_LABEL,
                ),
                ("TargetTrackingConfiguration.DisableScaleIn", "true"),
            ],
        ),
        (
            "the policy disabled, with a cooldown and a warmup",
            vec![
                ("Enabled", "false"),
                ("Cooldown", "300"),
                ("EstimatedInstanceWarmup", "120"),
            ],
        ),
    ]
}

/// Every value outside the set, in every shape of request, is refused with the published
/// set — and stores no policy.
#[tokio::test]
async fn the_published_set_is_stated_whatever_the_request_carried() {
    let router = router_with(new_engine());
    create_group(&router, "asg-enum-order").await;

    for (value_case, rejected) in values_outside_the_set() {
        for (shape_case, extra) in request_shapes() {
            let case = format!("{value_case}, with {shape_case}");
            let params = target_tracking_put("asg-enum-order", "track", rejected, &extra);
            let (status, body) =
                query(&router, "PutScalingPolicy", &borrowed(&params)).await;
            let (error_type, code, message) = refusal(&body);
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with {body:?}"
            );
            assert_eq!(
                (error_type.as_str(), code.as_str()),
                ("Sender", "ValidationError"),
                "case {case:?}: expected a Sender ValidationError, got {body:?}"
            );
            let expected = expected_message(rejected);
            assert_eq!(
                message, expected,
                "case {case:?}: expected the message {expected:?}, got {message:?}"
            );
        }
    }

    let (status, body) = query(
        &router,
        "DescribePolicies",
        &[("AutoScalingGroupName", "asg-enum-order")],
    )
    .await;
    assert_eq!(
        status, 200,
        "the read of the group's policies must succeed, got {status} with {body:?}"
    );
    assert_eq!(
        tag_value(&body, "PolicyName"),
        String::new(),
        "a refused put must store no policy, got {body:?}"
    );
}

/// The set the clause lists holds every value the model declares plus the one the service
/// publishes beyond it, and lists them in the order the table over those values is walked
/// — which is neither the model's declaration order nor a sort of the set, and is the same
/// whichever order the values are presented in.
#[tokio::test]
async fn the_stated_set_is_the_published_one_in_its_table_order() {
    let router = router_with(new_engine());
    create_group(&router, "asg-enum-derivation").await;

    let params = target_tracking_put(
        "asg-enum-derivation",
        "track",
        "ASGAverageDiskReads",
        &[],
    );
    let (status, body) = query(&router, "PutScalingPolicy", &borrowed(&params)).await;
    assert_eq!(
        status, 400,
        "a value outside the set must be refused, got {status} with {body:?}"
    );
    let (_, _, message) = refusal(&body);
    let stated = stated_values(&message);

    // The MEMBERS of the set: every value the model declares, plus the one the service
    // publishes beyond it, and nothing else.
    let mut published: Vec<&str> = DECLARED_METRIC_TYPES.to_vec();
    published.push(PUBLISHED_BEYOND_THE_MODEL);
    let mut expected_members = published.clone();
    expected_members.sort_unstable();
    let mut stated_members = stated.clone();
    stated_members.sort_unstable();
    assert_eq!(
        stated_members, expected_members,
        "case the stated set's members: expected {expected_members:?}, got {stated_members:?} \
         from {message:?}"
    );

    // The ORDER: the table's, computed from the values themselves, so the same set is
    // listed alike whichever order it is presented in.
    for (case, mut presented) in [
        ("the model's declaration order", published.clone()),
        ("that order reversed", {
            let mut reversed = published.clone();
            reversed.reverse();
            reversed
        }),
        ("the set sorted", {
            let mut sorted = published.clone();
            sorted.sort_unstable();
            sorted
        }),
    ] {
        presented.dedup();
        let derived = hashed_enum_order(&presented);
        assert_eq!(
            stated, derived,
            "case presented in {case:?}: the clause must list the table's order {derived:?}, \
             got {stated:?}"
        );
    }

    // And it is NEITHER of the two orders a reader might take it for, so a regression to
    // the framework's own rendering of the set fails here.
    assert_ne!(
        stated, published,
        "case the declaration order: the clause must not list the model's own order, got \
         {stated:?}"
    );
    assert_ne!(
        stated, expected_members,
        "case a sort of the set: the clause must not list the set sorted, got {stated:?}"
    );
}

/// A request that breaks a SECOND member's constraint is refused with both clauses, and
/// the enumerated member's clause still states the published set.
#[tokio::test]
async fn a_second_broken_member_does_not_move_the_stated_set() {
    let router = router_with(new_engine());
    create_group(&router, "asg-enum-two-faults").await;

    // `MetricAggregationType` is `XmlStringMaxLen32`
    // (`smithy_query(query="operation",operation="PutScalingPolicy")`), so a longer value
    // breaks its length bound.
    let over_long = "A".repeat(33);
    let rejected = "ASGAverageDiskReads";
    let mut params = target_tracking_put("asg-enum-two-faults", "track", rejected, &[]);
    params.push(p("MetricAggregationType", over_long.clone()));
    let (status, body) = query(&router, "PutScalingPolicy", &borrowed(&params)).await;
    let (error_type, code, message) = refusal(&body);
    assert_eq!(
        status, 400,
        "case two broken members: expected HTTP 400, got {status} with {body:?}"
    );
    assert_eq!(
        (error_type.as_str(), code.as_str()),
        ("Sender", "ValidationError"),
        "case two broken members: expected a Sender ValidationError, got {body:?}"
    );
    assert!(
        message.starts_with("2 validation errors detected: "),
        "case two broken members: the envelope must count both clauses, got {message:?}"
    );
    let clause = format!(
        "Value '{rejected}' at '{PREDEFINED_METRIC_TYPE_PATH}' failed to satisfy constraint: \
         Member must satisfy enum value set: [{PUBLISHED_METRIC_TYPES}]"
    );
    assert!(
        message.contains(&clause),
        "case two broken members: expected the clause {clause:?} inside {message:?}"
    );
    assert!(
        message.contains(&over_long),
        "case two broken members: the other clause must echo its own value, got {message:?}"
    );
}

/// Every value the model DOES declare is accepted and read back, so the refusals above
/// cannot pass by refusing every value.
#[tokio::test]
async fn every_declared_metric_type_is_accepted_and_read_back() {
    let router = router_with(new_engine());
    create_group(&router, "asg-enum-accepted").await;

    for accepted in DECLARED_METRIC_TYPES {
        // The request count per target is counted per target group, so that one metric
        // needs the resource label naming it.
        let extra: Vec<(&str, &str)> = if accepted == "ALBRequestCountPerTarget" {
            vec![(
                "TargetTrackingConfiguration.PredefinedMetricSpecification.ResourceLabel",
                RESOURCE_LABEL,
            )]
        } else {
            Vec::new()
        };
        let policy = format!("track-{accepted}");
        let params = target_tracking_put("asg-enum-accepted", &policy, accepted, &extra);
        let (status, body) = query(&router, "PutScalingPolicy", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {accepted:?}: expected HTTP 200, got {status} with {body:?}"
        );

        let (status, body) = query(
            &router,
            "DescribePolicies",
            &[
                ("AutoScalingGroupName", "asg-enum-accepted"),
                ("PolicyNames.member.1", policy.as_str()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {accepted:?}: the read back must succeed, got {status} with {body:?}"
        );
        assert_eq!(
            (
                tag_value(&body, "PolicyName").as_str(),
                tag_value(&body, "PredefinedMetricType").as_str()
            ),
            (policy.as_str(), accepted),
            "case {accepted:?}: the stored policy must carry the metric type it was put \
             with, got {body:?}"
        );
    }
}
