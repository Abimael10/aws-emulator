// SPDX-License-Identifier: CC0-1.0

//! How this front door answers a request value it could not READ — and which texts it CAN
//! read for a boolean member.
//!
//! The awsQuery front end has two layers of failure, and this suite is about the second one.
//! A value that was read and then refused (an absent required member, a value outside a
//! declared bound or outside an enum's set, a rule a handler applies) is this service's one
//! argument refusal, `ValidationError`, stated with the sentence that says what was wrong. A
//! member whose TEXT is no value of its wire type at all was never read, and WHICH value it
//! was decides how that is answered:
//!
//! 1. a BOOLEAN member's text is read from the lexical space the Query wire carries a boolean
//!    in — the two words and the two numerals that spell the same values, after the
//!    `whiteSpace = collapse` facet strips the padding around them — so every spelling of
//!    that space is a value this service reads and STORES;
//! 2. a boolean member's text outside that space is refused by the value layer that reads it,
//!    and this service states that refusal under its one argument-refusal code;
//! 3. every OTHER member type's text that is no value of its type — an integer, a double, a
//!    timestamp — keeps the front door's own code for a request it could not read,
//!    `MalformedInput`: to a client that is a different class (its own serialization is
//!    wrong) from a value the service read and refused, and the two are not merged into one
//!    code.
//!
//! Neither unreadable class states the reader's diagnostic, which is the parser's own report
//! of the text it could not read and the literals it accepts.
//!
//! The `<RequestId>` every one of these envelopes carries is `error_envelope.rs`'s subject,
//! not this suite's.

use crate::support::{borrowed, new_engine, p, query, router_with, tag_value, xml_unescape};

/// The one error code this service names for a problem with a request's arguments: no
/// operation of it models an input-validation fault
/// (`smithy_query(query="operation",operation="UpdateAutoScalingGroup")` declares only the
/// contention, activity-in-progress and service-linked-role faults), so an argument problem
/// is reported through the awsQuery common client error.
const ARGUMENT_REFUSAL_CODE: &str = "ValidationError";

/// The code the front door's own refusal of a request it could not read carries.
const UNREADABLE_VALUE_CODE: &str = "MalformedInput";

/// The launch source every group in this suite is created from: a launch TEMPLATE the test
/// harness puts in the account's own Amazon EC2 store.
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// The group this suite drives its updates against.
const GROUP: &str = "asg-argument-values";

/// Create [`GROUP`], so a refusal a handler raises is reached and an update that is accepted
/// has somewhere to land.
async fn group(router: &smithy_server_runtime::MultiServiceRouter) {
    let (status, body) = query(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", GROUP),
            LAUNCH_SOURCE,
            ("MinSize", "0"),
            ("MaxSize", "1"),
            ("AvailabilityZones.member.1", "us-west-2a"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "creating the suite's group must succeed, got {status} with {body:?}"
    );
}

/// The lexical space of an XML Schema boolean (W3C XML Schema Part 2, §3.2.2), as the Query
/// wire carries a boolean member's text: the two words, the two NUMERALS that spell the same
/// two values, and the padding the type's `whiteSpace = collapse` facet strips from around a
/// value.
///
/// The rows alternate between the two values, so a row that is read reports a value the row
/// before it did not.
const BOOLEAN_SPACE: &[(&str, &str, bool)] = &[
    ("the word true", "true", true),
    ("the word false", "false", false),
    ("the numeral one", "1", true),
    ("the numeral zero", "0", false),
    ("a word padded with spaces", " true ", true),
    ("a numeral padded with a tab and a newline", "\t0\n", false),
];

/// Every spelling of the lexical space is READ, and the boolean it spells is what a later
/// read of the group reports — a round trip through the store, not just an accepted request.
#[tokio::test]
async fn a_boolean_in_the_lexical_space_is_read_and_stored() {
    let router = router_with(new_engine());
    group(&router).await;

    for (case, text, expected) in BOOLEAN_SPACE {
        let (status, body) = query(
            &router,
            "UpdateAutoScalingGroup",
            &[("AutoScalingGroupName", GROUP), ("CapacityRebalance", text)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a spelling of the space must be read, got {status} with {body:?}"
        );
        let (status, body) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", GROUP)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read-back must succeed, got {status} with {body:?}"
        );
        let stored = tag_value(&body, "CapacityRebalance");
        assert_eq!(
            stored,
            expected.to_string(),
            "case {case:?}: expected the group to report {expected}, got {stored:?} from {body:?}"
        );
    }
}

/// One row of a refusal table: what the case is, the action, and the parameters it carries.
type RefusedRow<'a> = (&'a str, &'a str, Vec<(String, String)>);

/// One row of the boolean table: the operation, the boolean member an unreadable text is
/// sent for, and whatever else that request needs beside it.
type BooleanMember<'a> = (&'a str, &'a str, Vec<(String, String)>);

/// The texts a BOOLEAN member cannot be read from: a word of assent or refusal, the words in
/// another case, numbers outside the two the space holds, a decimal, a padded numeral, text
/// with whitespace INSIDE it (which the collapse facet does not strip) and no text at all.
const OUTSIDE_THE_BOOLEAN_SPACE: &[(&str, &str)] = &[
    ("a word of assent", "yes"),
    ("a word of refusal", "no"),
    ("the word in upper case", "TRUE"),
    ("the word in title case", "False"),
    ("a number outside the space", "2"),
    ("a negative number", "-1"),
    ("a padded numeral", "01"),
    ("a decimal", "1.0"),
    ("a word with a space inside it", "tr ue"),
    ("no text at all", ""),
];

/// Every BOOLEAN member this API reads, with whatever else its request needs beside it — one
/// per operation that carries a top-level boolean, plus one carried INSIDE a structure, so
/// the reading is exercised at every depth the wire has.
///
/// The members are the model's own
/// (`smithy_query(query="operation",operation="<the operation>")`).
fn boolean_members() -> Vec<BooleanMember<'static>> {
    vec![
        (
            "UpdateAutoScalingGroup",
            "CapacityRebalance",
            vec![p("AutoScalingGroupName", GROUP)],
        ),
        (
            "DeleteAutoScalingGroup",
            "ForceDelete",
            vec![p("AutoScalingGroupName", GROUP)],
        ),
        (
            "EnterStandby",
            "ShouldDecrementDesiredCapacity",
            vec![
                p("AutoScalingGroupName", GROUP),
                p("InstanceIds.member.1", "i-0123456789abcdef0"),
            ],
        ),
        (
            "TerminateInstanceInAutoScalingGroup",
            "ShouldDecrementDesiredCapacity",
            vec![p("InstanceId", "i-0123456789abcdef0")],
        ),
        (
            "SetInstanceHealth",
            "ShouldRespectGracePeriod",
            vec![
                p("InstanceId", "i-0123456789abcdef0"),
                p("HealthStatus", "Unhealthy"),
            ],
        ),
        (
            "CreateAutoScalingGroup",
            "NewInstancesProtectedFromScaleIn",
            vec![
                p("AutoScalingGroupName", "asg-protected"),
                p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
                p("MinSize", "0"),
                p("MaxSize", "1"),
                p("AvailabilityZones.member.1", "us-west-2a"),
            ],
        ),
        // A boolean INSIDE a structure: the member sits under
        // `TargetTrackingConfiguration`, so only a reader that walks the request reaches it.
        (
            "PutScalingPolicy",
            "TargetTrackingConfiguration.DisableScaleIn",
            vec![
                p("AutoScalingGroupName", GROUP),
                p("PolicyName", "track"),
                p("PolicyType", "TargetTrackingScaling"),
                p(
                    "TargetTrackingConfiguration.PredefinedMetricSpecification.\
                     PredefinedMetricType",
                    "ASGAverageCPUUtilization",
                ),
                p("TargetTrackingConfiguration.TargetValue", "40"),
            ],
        ),
    ]
}

/// The answer to a value this front door could not read: HTTP 400, `<Type>Sender`, the code
/// that class carries, and no reader's diagnostic.
fn assert_unreadable(case: &str, code: &str, status: u16, body: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {body:?}"
    );
    assert_eq!(
        (
            tag_value(body, "Type").as_str(),
            tag_value(body, "Code").as_str(),
            tag_value(body, "Message").as_str()
        ),
        ("Sender", code, ""),
        "case {case:?}: expected a Sender {code} with no message, got {body:?}"
    );
}

/// A boolean member's text outside the lexical space is this service's argument refusal, at
/// every boolean the API reads and for every spelling a client gets wrong.
#[tokio::test]
async fn a_boolean_outside_the_lexical_space_is_an_argument_refusal() {
    let router = router_with(new_engine());
    group(&router).await;

    for (action, member, extra) in boolean_members() {
        for (spelling, text) in OUTSIDE_THE_BOOLEAN_SPACE {
            let case = format!("{action}.{member} carrying {spelling}");
            let mut params = vec![p(member, *text)];
            params.extend(extra.iter().cloned());
            let (status, body) = query(&router, action, &borrowed(&params)).await;
            assert_unreadable(&case, ARGUMENT_REFUSAL_CODE, status, &body);
        }
    }
}

/// Every OTHER member type a request of this service carries a scalar of, sent a text that is
/// no value of that type: one row per type and per operation that reads one, including the
/// integer a group's health-check grace period is carried as and a double carried INSIDE a
/// structure.
fn unreadable_scalars() -> Vec<RefusedRow<'static>> {
    let mut rows: Vec<RefusedRow<'static>> = Vec::new();
    // The integer a group's health-check grace period is carried as, sent each text that is
    // no integer: a word, a decimal, an exponent form and a number with a thousands
    // separator.
    for text in ["abc", "1.5", "3e2", "1,800"] {
        rows.push((
            "an integer member whose text is no integer",
            "UpdateAutoScalingGroup",
            vec![
                p("AutoScalingGroupName", GROUP),
                p("HealthCheckGracePeriod", text),
            ],
        ));
    }
    rows.push((
        "another integer member of the same operation",
        "UpdateAutoScalingGroup",
        vec![
            p("AutoScalingGroupName", GROUP),
            p("MaxInstanceLifetime", "a-while"),
        ],
    ));
    rows.push((
        "an integer member of another operation",
        "SetDesiredCapacity",
        vec![
            p("AutoScalingGroupName", GROUP),
            p("DesiredCapacity", "not-an-integer"),
        ],
    ));
    rows.push((
        "a page size whose text is no integer",
        "DescribeAutoScalingGroups",
        vec![p("MaxRecords", "all-of-them")],
    ));
    rows.push((
        "an integer member of a create",
        "CreateAutoScalingGroup",
        vec![
            p("AutoScalingGroupName", "asg-unreadable-size"),
            p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
            p("MinSize", "none"),
            p("MaxSize", "1"),
            p("AvailabilityZones.member.1", "us-west-2a"),
        ],
    ));
    rows.push((
        "a double member inside a structure, whose text is no number",
        "PutScalingPolicy",
        vec![
            p("AutoScalingGroupName", GROUP),
            p("PolicyName", "track"),
            p("PolicyType", "TargetTrackingScaling"),
            p(
                "TargetTrackingConfiguration.PredefinedMetricSpecification.\
                 PredefinedMetricType",
                "ASGAverageCPUUtilization",
            ),
            p(
                "TargetTrackingConfiguration.TargetValue",
                "as-high-as-it-goes",
            ),
        ],
    ));
    rows.push((
        "a timestamp member whose text is no instant",
        "PutScheduledUpdateGroupAction",
        vec![
            p("AutoScalingGroupName", GROUP),
            p("ScheduledActionName", "scale-up"),
            p("StartTime", "tomorrow"),
            p("DesiredCapacity", "1"),
        ],
    ));
    rows
}

/// A value of any other type that the front door could not read keeps the front door's own
/// code: it is not restated as an argument refusal.
#[tokio::test]
async fn an_unreadable_value_of_any_other_type_keeps_the_front_doors_code() {
    let router = router_with(new_engine());
    group(&router).await;

    for (case, action, params) in unreadable_scalars() {
        let (status, body) = query(&router, action, &borrowed(&params)).await;
        assert_unreadable(case, UNREADABLE_VALUE_CODE, status, &body);
    }
}

/// The other layer is unmoved: a value the front door DID read and then refused — a rule the
/// model declares or one a handler applies — is this service's argument refusal, and it still
/// states the sentence that says what was wrong.
#[tokio::test]
async fn a_value_the_front_door_read_is_refused_with_its_own_sentence() {
    let router = router_with(new_engine());
    group(&router).await;

    let rows: Vec<RefusedRow<'_>> = vec![
        (
            "an absent required member",
            "SetDesiredCapacity",
            Vec::new(),
        ),
        (
            "a string longer than the bound the model declares",
            "UpdateAutoScalingGroup",
            vec![
                p("AutoScalingGroupName", GROUP),
                // `HealthCheckType` targets `XmlStringMaxLen32`, so 33 characters is one past
                // the declared bound: a value that WAS read and then refused.
                p("HealthCheckType", "E".repeat(33)),
            ],
        ),
        (
            "a readable string a handler refuses",
            "UpdateAutoScalingGroup",
            vec![
                p("AutoScalingGroupName", GROUP),
                p("HealthCheckType", "NOT-A-HEALTH-CHECK"),
            ],
        ),
        (
            "a readable integer a handler refuses",
            "UpdateAutoScalingGroup",
            vec![
                p("AutoScalingGroupName", GROUP),
                p("HealthCheckGracePeriod", "-1"),
            ],
        ),
        (
            "a readable page size outside the documented range",
            "DescribeAutoScalingGroups",
            vec![p("MaxRecords", "101")],
        ),
    ];

    for (case, action, params) in rows {
        let (status, body) = query(&router, action, &borrowed(&params)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            tag_value(&body, "Code"),
            ARGUMENT_REFUSAL_CODE,
            "case {case:?}: expected <Code>{ARGUMENT_REFUSAL_CODE}</Code>, got {body:?}"
        );
        let message = xml_unescape(&tag_value(&body, "Message"));
        assert!(
            !message.is_empty(),
            "case {case:?}: a value the service read is stated with a message, got {body:?}"
        );
    }
}
