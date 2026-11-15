// SPDX-License-Identifier: CC0-1.0

//! The request-validation message this service states an ENUM-constraint
//! violation in, driven end to end over its awsQuery HTTP surface with the
//! shared harness in `crate::harness`.
//!
//! RULE: a value outside a modelled enumerated value set is refused at the
//! boundary with `ValidationError` (400) and AWS's standard validation sentence,
//! whose only variable part is the rejected value echoed at the member's path:
//!
//! ```text
//! 1 validation error detected: Value '[<rejected>]' at 'attributeNames' failed to
//! satisfy constraint: Member must satisfy constraint: [Member must satisfy enum
//! value set: [<the value set, in the order the service publishes it>]]
//! ```
//!
//! The value set is part of that FIXED text, and the order the service publishes
//! it in is not the order the model declares the enum's members in — so the
//! sequence is written out here, as [`PUBLISHED_INSTANCES_HEALTH_ATTRIBUTES`],
//! and the test also pins it to be the same SET the model declares, read off the
//! generated constant.
//!
//! The rows span what the rule generalises over: how many values of the list are
//! rejected, whether a legal value sits beside a rejected one, the form of the
//! rejected value (an unknown name, a modelled name in the wrong case, an empty
//! value), and which environment selector — if any — the request carries, since
//! a request-shape refusal precedes every lookup.

use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;

use crate::harness::Emulator;
use crate::request_refusals::envelope;

/// The order this service publishes `InstancesHealthAttribute`'s value set in,
/// which is the order its validation message lists the set in.
///
/// Source: the service's own validation message for
/// `DescribeInstancesHealth$AttributeNames`. The SET is the model's
/// (`com.amazonaws.elasticbeanstalk#InstancesHealthAttribute`, generated as
/// `eb::ops::types::INSTANCES_HEALTH_ATTRIBUTE_VALUES`) and is asserted to be
/// here in full below; only the SEQUENCE is stated here, because the service
/// publishes it in neither the model's declaration order nor a sorted one.
const PUBLISHED_INSTANCES_HEALTH_ATTRIBUTES: &[&str] = &[
    "All",
    "Color",
    "Deployment",
    "LaunchedAt",
    "AvailabilityZone",
    "Causes",
    "RefreshedAt",
    "HealthStatus",
    "InstanceType",
    "System",
    "ApplicationMetrics",
];

/// The whole sentence the boundary refuses `rejected` — the elements of
/// `AttributeNames` outside the value set, in the order the request carried them
/// — with.
fn expected_message(rejected: &[&str]) -> String {
    format!(
        "{}Value '[{}]' at 'attributeNames' failed to satisfy constraint: Member must satisfy \
         constraint: [Member must satisfy enum value set: [{}]]",
        envelope(1),
        rejected.join(", "),
        PUBLISHED_INSTANCES_HEALTH_ATTRIBUTES.join(", ")
    )
}

/// One row: the `AttributeNames` the request carries, the other members it
/// carries, and which of its elements the boundary rejects.
struct AttributeCase {
    case: &'static str,
    attribute_names: &'static [&'static str],
    other: &'static [(&'static str, &'static str)],
    rejected: &'static [&'static str],
}

const CASES: &[AttributeCase] = &[
    AttributeCase {
        case: "one unknown value, with an environment that does not exist",
        attribute_names: &["NotARealAttribute"],
        other: &[("EnvironmentName", "health-ghost")],
        rejected: &["NotARealAttribute"],
    },
    AttributeCase {
        case: "one unknown value, with no environment selector at all",
        attribute_names: &["NotARealAttribute"],
        other: &[],
        rejected: &["NotARealAttribute"],
    },
    AttributeCase {
        case: "one unknown value, with an environment named by id",
        attribute_names: &["NotARealAttribute"],
        other: &[("EnvironmentId", "e-ghostghost")],
        rejected: &["NotARealAttribute"],
    },
    AttributeCase {
        case: "two unknown values of one list",
        attribute_names: &["NotARealAttribute", "AlsoNotReal"],
        other: &[("EnvironmentName", "health-ghost")],
        rejected: &["NotARealAttribute", "AlsoNotReal"],
    },
    AttributeCase {
        case: "a legal value beside an unknown one",
        attribute_names: &["All", "NotARealAttribute"],
        other: &[("EnvironmentName", "health-ghost")],
        rejected: &["NotARealAttribute"],
    },
    AttributeCase {
        case: "a modelled value in the wrong case",
        attribute_names: &["healthstatus"],
        other: &[("EnvironmentName", "health-ghost")],
        rejected: &["healthstatus"],
    },
    AttributeCase {
        case: "a modelled value with surrounding space",
        attribute_names: &["All "],
        other: &[("EnvironmentName", "health-ghost")],
        rejected: &["All "],
    },
];

/// RULE, over the rows above.
#[tokio::test]
async fn an_attribute_outside_the_value_set_is_refused_with_the_published_value_set() {
    let emulator = Emulator::new();

    for AttributeCase {
        case,
        attribute_names,
        other,
        rejected,
    } in CASES
    {
        let mut params: Vec<(String, String)> = other
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        for (index, value) in attribute_names.iter().enumerate() {
            params.push((
                format!("AttributeNames.member.{}", index + 1),
                (*value).to_string(),
            ));
        }
        let borrowed: Vec<(&str, &str)> = params
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let refused = emulator
            .call("DescribeInstancesHealth", &borrowed)
            .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400, got {} and body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "ValidationError",
            "case {case:?}: expected error code ValidationError, got {:?} in body {}",
            refused.error_code(),
            refused.body
        );
        let expected = expected_message(rejected);
        assert_eq!(
            refused.error_message(),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            refused.error_message()
        );
    }
}

/// The value set the message lists is the MODEL's set: the sequence above is an
/// order of the enum's members and nothing else, so a model that gains or loses a
/// member cannot leave the refusal naming a value the service rejects or omitting
/// one it accepts.
#[tokio::test]
async fn the_published_value_set_is_the_modelled_one() {
    let mut published: Vec<&str> = PUBLISHED_INSTANCES_HEALTH_ATTRIBUTES.to_vec();
    published.sort_unstable();
    let mut modelled: Vec<&str> = eb::ops::types::INSTANCES_HEALTH_ATTRIBUTE_VALUES.to_vec();
    modelled.sort_unstable();
    assert_eq!(
        published, modelled,
        "the published order must list exactly the modelled values, published {published:?} \
         against modelled {modelled:?}"
    );

    // The other side of the rule: every value the model declares passes the
    // boundary, so none of them is ever the rejected half of the sentence.
    let emulator = Emulator::new();
    for value in eb::ops::types::INSTANCES_HEALTH_ATTRIBUTE_VALUES {
        let answered = emulator
            .call(
                "DescribeInstancesHealth",
                &[
                    ("EnvironmentName", "health-ghost"),
                    ("AttributeNames.member.1", value),
                ],
            )
            .await;
        assert_ne!(
            answered.error_code(),
            "ValidationError",
            "case {value:?}: a modelled attribute name must not be refused as malformed, got {} \
             and body {}",
            answered.status,
            answered.body
        );
    }
}
