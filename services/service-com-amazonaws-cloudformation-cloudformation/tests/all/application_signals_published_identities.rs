// SPDX-License-Identifier: CC0-1.0

//! What a stack PUBLISHES as the identity of a CloudWatch Application Signals resource it
//! deployed: the value `Ref` resolves to, the attributes `Fn::GetAtt` reads, and the
//! `PhysicalResourceId` the stack reports for the logical id.
//!
//! RULE: the identity a stack publishes for a resource is its TYPE's primary identifier — the
//! single published value that names one resource of the type where the type has one, and the
//! parts of a COMPOSITE primary identifier joined with `|` in the order the type's schema states
//! them where it has several — and every other value the type publishes is a `Fn::GetAtt`
//! attribute beside it, never the identity itself. A resource whose identity is published
//! therefore resolves `Ref` and every `Fn::GetAtt` of it to a value, so each entry of a settled
//! stack's `Outputs` carries an `OutputValue`: the dict comprehension every client builds over
//! `Outputs` is what a bare key raises on.
//!
//! The rows span what the rule generalises over: WHICH of the three types the template declares,
//! the FORM of the identity (the account whose singleton the resource is, a composite of five
//! parts, and an ARN the owning service mints), the values the declaration states those parts
//! from, and which published return value each output reads.
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs` builds,
//! so what is asserted is what a client that deploys the template and then reads the stack
//! observes.

use serde_json::{json, Value};

use crate::application_signals_resource_types::{
    deployed, minimal_capture, output, GROUPING_CONFIGURATION, INSTRUMENTATION_CONFIG, SLO,
    SNAPSHOT,
};
use crate::harness::{physical_resource_id, router};
use crate::stack_lifecycle::ACCOUNT_ID;

/// The separator CloudFormation joins the parts of a composite primary identifier with.
const IDENTITY_SEPARATOR: &str = "|";

/// How many characters an instrumentation configuration's `LocationHash` carries, from that
/// member's own `@length(min: 16, max: 16)`
/// (`smithy_query(query="operation",operation="CreateInstrumentationConfiguration")`).
const LOCATION_HASH_CHARS: usize = 16;

/// What one row expects the stack to publish as the resource's identity.
enum Identity {
    /// The ACCOUNT whose singleton the resource is, which the type also publishes as
    /// `AccountId`.
    Account,
    /// The COMPOSITE of the four dimensions the declaration states and the `LocationHash` the
    /// owning service computed, which the output named here carries.
    Composite {
        parts: &'static [&'static str],
        last_part_output: &'static str,
    },
    /// The ARN the owning service minted, which the output named here carries.
    Arn { output: &'static str },
}

/// One declaration, the outputs it publishes and the identity the rule gives it.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    resource_type: &'static str,
    properties: Value,
    /// Every published return value the template reads, as `(output key, Fn::GetAtt attribute)`.
    attributes: &'static [(&'static str, &'static str)],
    identity: Identity,
}

/// RULE: `Ref` resolves to the type's primary identifier, and every other published return value
/// is an attribute beside it — so every output of a settled stack carries a value, and the
/// `PhysicalResourceId` the stack reports is that same identifier.
#[tokio::test]
async fn a_stack_publishes_each_resources_own_primary_identifier() {
    let rows = vec![
        Row {
            case: "the account's grouping configuration, one definition",
            stack_name: "appsig-identity-grouping-one",
            resource_type: GROUPING_CONFIGURATION,
            properties: json!({
                "GroupingAttributeDefinitions": [
                    {
                        "GroupingName": "BusinessUnit",
                        "GroupingSourceKeys": ["aws.local.service"],
                        "DefaultGroupingValue": "unassigned",
                    },
                ],
            }),
            attributes: &[("AccountId", "AccountId"), ("UpdatedAt", "UpdatedAt")],
            identity: Identity::Account,
        },
        Row {
            // The same type declared differently: the identity is the account's whatever the
            // declaration states, because the account IS the identifier.
            case: "the account's grouping configuration, two definitions",
            stack_name: "appsig-identity-grouping-two",
            resource_type: GROUPING_CONFIGURATION,
            properties: json!({
                "GroupingAttributeDefinitions": [
                    { "GroupingName": "Team", "GroupingSourceKeys": ["team.name"] },
                    { "GroupingName": "CostCentre", "DefaultGroupingValue": "shared" },
                ],
            }),
            attributes: &[("AccountId", "AccountId")],
            identity: Identity::Account,
        },
        Row {
            case: "a probe instrumentation point",
            stack_name: "appsig-identity-probe",
            resource_type: INSTRUMENTATION_CONFIG,
            properties: json!({
                "InstrumentationType": "PROBE",
                "Service": "checkout",
                "Environment": "ec2:production",
                "SignalType": SNAPSHOT,
                "Location": {
                    "CodeLocation": {
                        "Language": "Python",
                        "CodeUnit": "app.checkout.cart",
                        "MethodName": "total",
                        "FilePath": "app/checkout/cart.py",
                    },
                },
                "CaptureConfiguration": { "CodeCapture": { "CaptureLimits": { "MaxHits": 10 } } },
            }),
            attributes: &[
                ("Arn", "Arn"),
                ("CreatedAt", "CreatedAt"),
                ("LocationHash", "LocationHash"),
            ],
            identity: Identity::Composite {
                parts: &["PROBE", "checkout", "ec2:production", SNAPSHOT],
                last_part_output: "LocationHash",
            },
        },
        Row {
            // Another point: the parts the declaration states are other values, in another
            // Environment spelling, and the type is the other one the model publishes.
            case: "a breakpoint instrumentation point in a Kubernetes environment",
            stack_name: "appsig-identity-breakpoint",
            resource_type: INSTRUMENTATION_CONFIG,
            properties: json!({
                "InstrumentationType": "BREAKPOINT",
                "Service": "inventory",
                "Environment": "eks:cluster-prod/namespace",
                "SignalType": SNAPSHOT,
                "Description": "the reservation path",
                "Location": {
                    "CodeLocation": {
                        "Language": "Java",
                        "CodeUnit": "com.example.inventory",
                        "ClassName": "StockService",
                        "MethodName": "reserve",
                        "FilePath": "src/main/java/com/example/StockService.java",
                        "LineNumber": 88,
                    },
                },
                "CaptureConfiguration": minimal_capture(),
            }),
            attributes: &[("Arn", "Arn"), ("LocationHash", "LocationHash")],
            identity: Identity::Composite {
                parts: &[
                    "BREAKPOINT",
                    "inventory",
                    "eks:cluster-prod/namespace",
                    SNAPSHOT,
                ],
                last_part_output: "LocationHash",
            },
        },
        Row {
            // The other arm of the rule: a type whose primary identifier is ONE published value,
            // the ARN that service builds around the objective's own name.
            case: "a service level objective, identified by its ARN",
            stack_name: "appsig-identity-objective",
            resource_type: SLO,
            properties: json!({
                "Name": "published-identity-objective",
                "Sli": {
                    "SliMetric": {
                        "KeyAttributes": {
                            "Type": "Service",
                            "Name": "checkout",
                            "Environment": "ec2:production",
                        },
                        "MetricType": "LATENCY",
                        "PeriodSeconds": 60,
                        "Statistic": "p99",
                    },
                    "MetricThreshold": 150.0,
                    "ComparisonOperator": "LessThan",
                },
            }),
            attributes: &[("Arn", "Arn"), ("CreatedTime", "CreatedTime")],
            identity: Identity::Arn { output: "Arn" },
        },
    ];

    for row in rows {
        let router = router();
        let case = row.case;
        let mut outputs = serde_json::Map::new();
        outputs.insert("Ref".to_string(), json!({ "Value": { "Ref": "Declared" } }));
        for (key, attribute) in row.attributes {
            outputs.insert(
                (*key).to_string(),
                json!({ "Value": { "Fn::GetAtt": ["Declared", attribute] } }),
            );
        }
        let template = json!({
            "Resources": {
                "Declared": { "Type": row.resource_type, "Properties": row.properties }
            },
            "Outputs": Value::Object(outputs)
        })
        .to_string();

        let described = deployed(&router, case, row.stack_name, &template).await;
        // Every published return value the template reads carries a value: a bare OutputKey is
        // what a client indexing OutputValue raises on.
        let published = output(case, &described, "Ref");
        let read: Vec<(&str, String)> = row
            .attributes
            .iter()
            .map(|(key, _)| (*key, output(case, &described, key)))
            .collect();
        let value_of = |key: &str| {
            read.iter()
                .find(|(named, _)| *named == key)
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| panic!("case {case:?}: the row reads an {key} output"))
        };

        match row.identity {
            Identity::Account => {
                assert_eq!(
                    published, ACCOUNT_ID,
                    "case {case:?}: expected Ref to return the account whose singleton the \
                     resource is, got {published}"
                );
                assert_eq!(
                    value_of("AccountId"),
                    published,
                    "case {case:?}: expected AccountId to publish the same account, got {read:?}"
                );
                assert!(
                    published.len() == 12 && published.chars().all(|digit| digit.is_ascii_digit()),
                    "case {case:?}: expected a 12-digit account id, got {published}"
                );
            }
            Identity::Composite {
                parts,
                last_part_output,
            } => {
                let last = value_of(last_part_output);
                let expected = parts
                    .iter()
                    .copied()
                    .chain(std::iter::once(last.as_str()))
                    .collect::<Vec<&str>>()
                    .join(IDENTITY_SEPARATOR);
                assert_eq!(
                    published, expected,
                    "case {case:?}: expected Ref to return the composite primary identifier \
                     {expected:?}, got {published:?}"
                );
                assert!(
                    last.len() == LOCATION_HASH_CHARS
                        && last.chars().all(|held| held.is_ascii_hexdigit()),
                    "case {case:?}: expected the {LOCATION_HASH_CHARS}-character location hash \
                     that service computed, got {last:?}"
                );
                // The ARN is a value of its own, not the identity: it carries the same hash and
                // is read through its own attribute.
                let arn = value_of("Arn");
                assert_ne!(
                    arn, published,
                    "case {case:?}: expected the ARN to be published beside the identity rather \
                     than as it, got {arn}"
                );
                assert!(
                    arn.starts_with("arn:aws:application-signals:") && arn.ends_with(&last),
                    "case {case:?}: expected the ARN to carry the location hash, got {arn}"
                );
            }
            Identity::Arn { output: named } => {
                let arn = value_of(named);
                assert_eq!(
                    published, arn,
                    "case {case:?}: expected Ref to return the ARN that identifies the resource, \
                     got {published}"
                );
                assert!(
                    arn.starts_with("arn:aws:application-signals:"),
                    "case {case:?}: expected that service's own ARN, got {arn}"
                );
            }
        }

        // Each value the type publishes is a value of its own: none of them is the stand-in a
        // return value no service has reported yet carries.
        for (key, value) in &read {
            assert!(
                !value.contains("is not available"),
                "case {case:?}: expected the {key} the owning service reported, got {value}"
            );
        }

        // The identifier the stack REPORTS for the resource is the same one: "the name or unique
        // identifier associated with the physical instance of the resource".
        let reported = physical_resource_id(&router, case, row.stack_name, "Declared").await;
        assert_eq!(
            reported, published,
            "case {case:?}: expected the reported PhysicalResourceId to be the identity Ref \
             resolves to, got {reported}"
        );
    }
}

/// One type's catalogued identity: the parts of its composite primary identifier (empty for a
/// type identified by one value), the properties a client names one of its resources by, and the
/// return values the type publishes.
struct Catalogued {
    resource_type: &'static str,
    composite: &'static [&'static str],
    identity: &'static [&'static str],
    attributes: &'static [&'static str],
}

/// The same rule in the catalogue the stack publishes from: which properties identify one
/// resource of each type, and which values the type publishes beside them.
///
/// The rows span the same three forms the deployed table does — a composite identifier, the
/// account's singleton and an ARN — so the catalogue cannot drift from what a stack reports.
#[test]
fn each_types_catalogued_identity_is_the_one_a_stack_publishes() {
    use service_com_amazonaws_cloudformation_cloudformation::state::primary_identifier;
    use service_com_amazonaws_cloudformation_cloudformation::state::template::{
        composite_identity_properties, published_attribute_names,
    };

    /// The five values `GetInstrumentationConfiguration` and
    /// `DeleteInstrumentationConfiguration` each take to address one configuration.
    const INSTRUMENTATION_IDENTITY: &[&str] = &[
        "InstrumentationType",
        "Service",
        "Environment",
        "SignalType",
        "LocationHash",
    ];

    let rows = &[
        Catalogued {
            resource_type: INSTRUMENTATION_CONFIG,
            composite: INSTRUMENTATION_IDENTITY,
            identity: INSTRUMENTATION_IDENTITY,
            attributes: &["Arn", "CreatedAt", "LocationHash"],
        },
        Catalogued {
            resource_type: GROUPING_CONFIGURATION,
            // Not composite: the account whose singleton the configuration is identifies it.
            composite: &[],
            identity: &["AccountId"],
            attributes: &["AccountId", "UpdatedAt"],
        },
        Catalogued {
            resource_type: SLO,
            composite: &[],
            identity: &["Arn"],
            attributes: &[
                "Arn",
                "CreatedTime",
                "EvaluationType",
                "LastUpdatedTime",
                "MetricSourceType",
            ],
        },
    ];

    for row in rows {
        let resource_type = row.resource_type;
        let (composite, identity, attributes) = (row.composite, row.identity, row.attributes);
        assert_eq!(
            composite_identity_properties(resource_type),
            composite,
            "case {resource_type:?}: expected the composite identifier {composite:?}, got {:?}",
            composite_identity_properties(resource_type)
        );
        let identifies: Vec<String> = identity.iter().map(|part| (*part).to_string()).collect();
        assert_eq!(
            primary_identifier::properties(resource_type),
            identifies,
            "case {resource_type:?}: expected the primary identifier {identity:?}, got {:?}",
            primary_identifier::properties(resource_type)
        );
        assert_eq!(
            published_attribute_names(resource_type),
            attributes,
            "case {resource_type:?}: expected the published return values {attributes:?}, got {:?}",
            published_attribute_names(resource_type)
        );
    }
}
