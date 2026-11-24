// SPDX-License-Identifier: CC0-1.0

//! How an option SPECIFICATION is resolved against the configuration-option
//! catalogue a platform version publishes, and how a requirement one option's
//! published description states in terms of ANOTHER option is reported — driven
//! end to end over the service's awsQuery HTTP surface.
//!
//! The rules under test:
//!
//! * a `(Namespace, OptionName)` pair is resolved against the catalogue the
//!   platform version publishes: inside a namespace whose option NAMES that
//!   catalogue states (one it publishes any option of), a name it does not
//!   publish names no configuration setting, so the specification earns one
//!   error-severity `ValidationMessage` saying the setting is unknown — echoing
//!   the namespace and the option name the caller sent, whatever value it
//!   carried — while a namespace the catalogue states no names of resolves a
//!   specification whatever the option in it is called;
//! * an operation that WRITES such a specification is refused
//!   `ConfigurationValidationException` with the service's configuration-
//!   validation sentence carrying that same text, and stores nothing;
//! * a requirement an option's published description states in terms of ANOTHER
//!   option's value is a CO-REQUIREMENT between the two rather than a bound of
//!   either one: the Auto Scaling group's `MinSize` must be "no greater than
//!   MaxSize", so an inverted pair of values that are each inside their own
//!   option's published `MinValue`..`MaxValue` range is reported on BOTH halves,
//!   each naming the co-requirement the pair broke, and a pair in order is
//!   reported on neither; the per-option range check keeps reporting the
//!   published bound alone.
//!
//! The namespaces and option names the rows are built from are read from the
//! catalogue the service publishes (`eb::state::published_configuration_options`,
//! what `DescribeConfigurationOptions` reports), so no row asserts a name or a
//! bound this service does not publish, and the unknown-name rule is asserted on
//! EVERY namespace that catalogue describes rather than on one of them.
//!
//! The HTTP harness (`Emulator`, `Answer`) is the one `all/lifecycle.rs` owns in
//! this same test binary, and the option-setting encoder the one
//! `all/configuration_templates.rs` owns, so this suite keeps no second copy of
//! either.

use crate::configuration_templates::{as_params, created, option_params, published_stack};
use crate::harness::{Answer, Emulator};
use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;

/// The application the configuration sets below belong to.
const APPLICATION: &str = "specification-app";
/// The stored configuration template the specifications are resolved against.
const TEMPLATE: &str = "specification-template";

/// The Auto Scaling group namespace and its two size options, as the service's
/// own catalogue names them.
const ASG: &str = "aws:autoscaling:asg";
/// See [`ASG`].
const MIN_SIZE: &str = "MinSize";
/// See [`ASG`].
const MAX_SIZE: &str = "MaxSize";

/// The two clauses the group's size co-requirement is reported with, one per
/// side of the pair — word for word as the service states them.
const MIN_ABOVE_MAX: &str = "MinSize is greater than MaxSize";
/// See [`MIN_ABOVE_MAX`].
const MAX_BELOW_MIN: &str = "MaxSize is less than MinSize";

/// The maximum size the template below carries, which is the other half of the
/// co-requirement for a request that sends only the minimum.
const STORED_MAX_SIZE: i64 = 9;

/// One `ValidationMessage` as the answer reports it, read member by member off
/// ONE element so an answer that attributes a message to the wrong option fails
/// rather than matching independent flat lists.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedMessage {
    namespace: String,
    option_name: String,
    severity: String,
    message: String,
}

/// The error-severity messages in `answer`, in the order the document lists
/// them: the findings about a specification or a value, read apart from the
/// advisories the resulting configuration carries.
fn reported_errors(answer: &Answer) -> Vec<ReportedMessage> {
    answer
        .elements_at("Messages/member")
        .iter()
        .map(|member| ReportedMessage {
            namespace: member.value("Namespace").unwrap_or_default().to_string(),
            option_name: member.value("OptionName").unwrap_or_default().to_string(),
            severity: member.value("Severity").unwrap_or_default().to_string(),
            message: member
                .value("Message")
                .unwrap_or_default()
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&amp;", "&"),
        })
        .filter(|message| message.severity == "error")
        .collect()
}

/// The catalogue the platform version the fixtures run publishes — the one
/// `DescribeConfigurationOptions` reports for it, which is what a specification
/// is resolved against.
fn catalogue() -> Vec<&'static eb::state::ConfigurationOption> {
    eb::state::published_configuration_options(Some(published_stack(0)), true)
}

/// The published description of `namespace` / `option_name` on that platform
/// version, so a row states no bound this service does not publish.
fn published(namespace: &str, option_name: &str) -> &'static eb::state::ConfigurationOption {
    catalogue()
        .into_iter()
        .find(|option| option.namespace == namespace && option.name == option_name)
        .unwrap_or_else(|| panic!("the catalogue publishes {namespace} / {option_name}"))
}

/// Every namespace that catalogue publishes an option of — the namespaces whose
/// option NAMES it states, each once.
fn described_namespaces() -> Vec<&'static str> {
    let mut namespaces: Vec<&'static str> =
        catalogue().iter().map(|option| option.namespace).collect();
    namespaces.sort_unstable();
    namespaces.dedup();
    namespaces
}

/// The sentence a specification the platform cannot resolve is reported with,
/// word for word, with the namespace and the option name the request sent in it.
fn unknown_specification(namespace: &str, option_name: &str) -> String {
    format!(
        "Invalid option specification (Namespace: '{namespace}', OptionName: \
         '{option_name}'): Unknown configuration setting."
    )
}

/// The sentence a message about `value` for `namespace` / `option_name` opens
/// with: the value the request sent and the option it was sent for.
fn invalid_value(namespace: &str, option_name: &str, value: &str, requirement: &str) -> String {
    format!(
        "Invalid option value: '{value}' (Namespace: '{namespace}', OptionName: \
         '{option_name}'): {requirement}"
    )
}

/// What this service refuses a configuration it cannot accept with, ahead of the
/// validation message that says which specification or value was not accepted.
fn configuration_validation(message: &str) -> String {
    format!("Configuration validation exception: {message}")
}

/// An emulator holding the application and the template the rows resolve
/// against. The template carries a `MaxSize` of its own, so a row can show the
/// co-requirement resolved against the configuration set's STORED half.
async fn emulator_with_template() -> Emulator {
    let emulator = Emulator::new();
    created(
        &emulator,
        "CreateApplication",
        &[("ApplicationName", APPLICATION)],
    )
    .await;
    let mut params = vec![
        ("ApplicationName".to_string(), APPLICATION.to_string()),
        ("TemplateName".to_string(), TEMPLATE.to_string()),
        ("SolutionStackName".to_string(), published_stack(0).name()),
    ];
    params.extend(option_params(&[(
        ASG,
        MAX_SIZE,
        &STORED_MAX_SIZE.to_string(),
        "",
    )]));
    created(
        &emulator,
        "CreateConfigurationTemplate",
        &as_params(&params),
    )
    .await;
    emulator
}

/// Validate `settings` against the stored template, requiring a 200: the
/// evaluation ANSWERS, and the messages are what it answers with.
async fn validated(emulator: &Emulator, settings: &[(&str, &str, &str, &str)]) -> Answer {
    let mut params = vec![
        ("ApplicationName".to_string(), APPLICATION.to_string()),
        ("TemplateName".to_string(), TEMPLATE.to_string()),
    ];
    params.extend(option_params(settings));
    created(
        emulator,
        "ValidateConfigurationSettings",
        &as_params(&params),
    )
    .await
}

/// The values the template holds for `namespace` / `option_name`, read back
/// through `DescribeConfigurationSettings` — the store's own answer, so a write
/// that was refused and still landed fails the row.
async fn stored_values(
    emulator: &Emulator,
    namespace: &str,
    option_name: &str,
) -> Vec<String> {
    let settings = created(
        emulator,
        "DescribeConfigurationSettings",
        &[("ApplicationName", APPLICATION), ("TemplateName", TEMPLATE)],
    )
    .await;
    settings
        .elements_at("OptionSettings/member")
        .iter()
        .filter(|member| {
            member.value("Namespace") == Some(namespace)
                && member.value("OptionName") == Some(option_name)
        })
        .filter_map(|member| member.value("Value").map(str::to_string))
        .collect()
}

#[tokio::test]
async fn an_option_name_the_published_namespace_does_not_hold_is_reported_as_unknown() {
    let emulator = emulator_with_template().await;

    // EVERY namespace the catalogue describes, each sent a name it does not
    // publish: the rule is the catalogue's, not one namespace's, so a namespace
    // whose names were resolved from nothing fails here.
    let unresolvable = "NotAnOptionInThisNamespace";
    for namespace in described_namespaces() {
        assert!(
            !catalogue()
                .iter()
                .any(|option| option.namespace == namespace && option.name == unresolvable),
            "case {namespace:?}: expected {unresolvable:?} to be absent from the published \
             catalogue"
        );
        let answer = validated(&emulator, &[(namespace, unresolvable, "2", "")]).await;
        assert_eq!(
            reported_errors(&answer),
            vec![ReportedMessage {
                namespace: namespace.to_string(),
                option_name: unresolvable.to_string(),
                severity: "error".to_string(),
                message: unknown_specification(namespace, unresolvable),
            }],
            "case {namespace:?}: expected one error-severity message about the unresolved \
             specification, got {}",
            answer.body
        );
    }
}

/// One row of the name table: how the option name the request sends misses the
/// names the namespace publishes, or why it resolves.
struct NameCase {
    case: &'static str,
    namespace: &'static str,
    option_name: String,
    /// The value the specification carries — an unresolved specification is
    /// reported whatever it is, including a value its namesake option would
    /// itself refuse.
    value: &'static str,
    resolved: bool,
}

#[tokio::test]
async fn an_option_name_is_resolved_against_the_names_its_namespace_publishes() {
    let emulator = emulator_with_template().await;

    // The names the rows are built from are read from the published catalogue:
    // one it publishes for the namespace, one it publishes for ANOTHER
    // namespace, and the case-shifted spelling of a name it does publish.
    let published_here = published(ASG, MIN_SIZE).name;
    let published_elsewhere = published("aws:ec2:vpc", "VPCId").name;
    let cases = vec![
        NameCase {
            case: "a name the namespace publishes",
            namespace: ASG,
            option_name: published_here.to_string(),
            value: "2",
            resolved: true,
        },
        NameCase {
            case: "a name no namespace publishes",
            namespace: ASG,
            option_name: "NotAnOptionInThisNamespace".to_string(),
            value: "2",
            resolved: false,
        },
        NameCase {
            case: "a name another namespace publishes",
            namespace: ASG,
            option_name: published_elsewhere.to_string(),
            value: "vpc-0123456789abcdef0",
            resolved: false,
        },
        NameCase {
            case: "the case-shifted spelling of a published name",
            namespace: ASG,
            option_name: published_here.to_lowercase(),
            value: "2",
            resolved: false,
        },
        NameCase {
            case: "an unresolved name whose value its namesake would also refuse",
            namespace: ASG,
            option_name: "NotAnOptionInThisNamespace".to_string(),
            value: "not-a-number",
            resolved: false,
        },
        NameCase {
            case: "a name the caller chooses, in a namespace that publishes none",
            namespace: "aws:elasticbeanstalk:application:environment",
            option_name: "STAGE".to_string(),
            value: "qa",
            resolved: true,
        },
        NameCase {
            case: "a name in a namespace scoped to a resource of its own",
            namespace: "aws:autoscaling:scheduledaction",
            option_name: "DesiredCapacity".to_string(),
            value: "3",
            resolved: true,
        },
        NameCase {
            case: "a name in a namespace this service publishes no option of",
            namespace: "aws:rds:dbinstance",
            option_name: "DBAllocatedStorage".to_string(),
            value: "10",
            resolved: true,
        },
        NameCase {
            case: "a name in a resource-scoped namespace, after its resource",
            namespace: "aws:elbv2:listener:443",
            option_name: "Protocol".to_string(),
            value: "HTTPS",
            resolved: true,
        },
    ];

    for case in &cases {
        let answer = validated(
            &emulator,
            &[(case.namespace, case.option_name.as_str(), case.value, "")],
        )
        .await;
        let reported = reported_errors(&answer);
        if case.resolved {
            assert!(
                reported.is_empty(),
                "case {:?}: expected no error-severity message, got {reported:?}",
                case.case
            );
            continue;
        }
        assert_eq!(
            reported,
            vec![ReportedMessage {
                namespace: case.namespace.to_string(),
                option_name: case.option_name.clone(),
                severity: "error".to_string(),
                message: unknown_specification(case.namespace, &case.option_name),
            }],
            "case {:?}: expected one error-severity message about the unresolved specification, \
             got {reported:?}",
            case.case
        );
    }
}

#[tokio::test]
async fn a_write_of_an_unresolved_option_name_is_refused_and_stores_nothing() {
    let emulator = emulator_with_template().await;
    let unresolvable = "NotAnOptionInThisNamespace";

    let mut params = vec![
        ("ApplicationName".to_string(), APPLICATION.to_string()),
        ("TemplateName".to_string(), TEMPLATE.to_string()),
    ];
    params.extend(option_params(&[(ASG, unresolvable, "2", "")]));
    let refused = emulator
        .call("UpdateConfigurationTemplate", &as_params(&params))
        .await;
    assert_eq!(
        (refused.status, refused.error_code()),
        (400, "ConfigurationValidationException"),
        "case \"a write of an unresolved option name\": expected HTTP 400 \
         ConfigurationValidationException, got {} and body {}",
        refused.status,
        refused.body
    );
    assert_eq!(
        refused.error_message(),
        configuration_validation(&unknown_specification(ASG, unresolvable)),
        "case \"a write of an unresolved option name\": expected the configuration-validation \
         sentence carrying the unknown-setting text, got {}",
        refused.body
    );
    assert!(
        stored_values(&emulator, ASG, unresolvable).await.is_empty(),
        "case \"a write of an unresolved option name\": expected the refusal to store nothing, \
         got {:?}",
        stored_values(&emulator, ASG, unresolvable).await
    );
    assert_eq!(
        stored_values(&emulator, ASG, MAX_SIZE).await,
        vec![STORED_MAX_SIZE.to_string()],
        "case \"a write of an unresolved option name\": expected the template's own settings to \
         be left as they were"
    );
}

/// One row of the co-requirement table: the pair of Auto Scaling group sizes the
/// request sends, and whether their ORDER keeps the requirement between them.
struct PairCase {
    case: &'static str,
    minimum: i64,
    maximum: i64,
    inverted: bool,
}

#[tokio::test]
async fn an_inverted_group_size_pair_is_reported_as_the_co_requirement_it_breaks() {
    let emulator = emulator_with_template().await;

    // The rows differ along HOW the pair sits: inverted by one, inverted at the
    // published floor, inverted at the published ceiling, equal (which "no
    // greater than MaxSize" admits), and in order.
    let min_size = published(ASG, MIN_SIZE);
    let max_size = published(ASG, MAX_SIZE);
    let floor = i64::from(
        min_size
            .min_value
            .expect("the catalogue publishes a MinValue for the group's minimum size"),
    );
    let ceiling = i64::from(
        max_size
            .max_value
            .expect("the catalogue publishes a MaxValue for the group's maximum size"),
    );
    let cases = vec![
        PairCase {
            case: "inverted by two",
            minimum: 4,
            maximum: 2,
            inverted: true,
        },
        PairCase {
            case: "inverted by one",
            minimum: 3,
            maximum: 2,
            inverted: true,
        },
        PairCase {
            case: "inverted at the published floor",
            minimum: floor + 1,
            maximum: floor,
            inverted: true,
        },
        PairCase {
            case: "inverted at the published ceiling",
            minimum: ceiling,
            maximum: ceiling - 1,
            inverted: true,
        },
        PairCase {
            case: "equal halves",
            minimum: 3,
            maximum: 3,
            inverted: false,
        },
        PairCase {
            case: "in order",
            minimum: 2,
            maximum: 4,
            inverted: false,
        },
    ];

    for case in &cases {
        // Each half is inside the range its OWN published description states,
        // so nothing the row asserts is a per-option range violation.
        for (option, value) in [(min_size, case.minimum), (max_size, case.maximum)] {
            assert!(
                option.min_value.is_none_or(|minimum| value >= i64::from(minimum))
                    && option.max_value.is_none_or(|maximum| value <= i64::from(maximum)),
                "case {:?}: expected {value} to be inside the published range of {:?}, which is \
                 {:?}..{:?}",
                case.case,
                option.name,
                option.min_value,
                option.max_value
            );
        }

        let minimum = case.minimum.to_string();
        let maximum = case.maximum.to_string();
        let answer = validated(
            &emulator,
            &[(ASG, MIN_SIZE, &minimum, ""), (ASG, MAX_SIZE, &maximum, "")],
        )
        .await;
        let reported = reported_errors(&answer);
        if !case.inverted {
            assert!(
                reported.is_empty(),
                "case {:?}: expected no error-severity message, got {reported:?}",
                case.case
            );
            continue;
        }
        assert_eq!(
            reported,
            vec![
                ReportedMessage {
                    namespace: ASG.to_string(),
                    option_name: MIN_SIZE.to_string(),
                    severity: "error".to_string(),
                    message: invalid_value(ASG, MIN_SIZE, &minimum, MIN_ABOVE_MAX),
                },
                ReportedMessage {
                    namespace: ASG.to_string(),
                    option_name: MAX_SIZE.to_string(),
                    severity: "error".to_string(),
                    message: invalid_value(ASG, MAX_SIZE, &maximum, MAX_BELOW_MIN),
                },
            ],
            "case {:?}: expected one message per half, each naming the co-requirement the pair \
             broke, got {reported:?}",
            case.case
        );
    }
}

#[tokio::test]
async fn a_half_outside_its_own_published_range_is_reported_against_that_bound() {
    let emulator = emulator_with_template().await;

    // A per-option range violation names the bound the catalogue publishes for
    // THAT option, never a value read off the other half: the minimum below is
    // above the maximum in force as well, and what it is reported against is
    // still its own published ceiling.
    let ceiling = i64::from(
        published(ASG, MIN_SIZE)
            .max_value
            .expect("the catalogue publishes a MaxValue for the group's minimum size"),
    );
    let above = (ceiling + 1).to_string();
    let answer = validated(&emulator, &[(ASG, MIN_SIZE, &above, "")]).await;
    assert_eq!(
        reported_errors(&answer),
        vec![ReportedMessage {
            namespace: ASG.to_string(),
            option_name: MIN_SIZE.to_string(),
            severity: "error".to_string(),
            message: invalid_value(
                ASG,
                MIN_SIZE,
                &above,
                &format!("Value exceeds maximum allowed value: {ceiling}"),
            ),
        }],
        "case \"one past the published ceiling\": expected the published bound in the message, \
         got {}",
        answer.body
    );

    // The same option, inside its published range but above the maximum the
    // TEMPLATE carries: the other half of the co-requirement comes from the
    // configuration in force, so the half the request sent is reported against
    // the co-requirement instead.
    let above_stored = (STORED_MAX_SIZE + 1).to_string();
    let answer = validated(&emulator, &[(ASG, MIN_SIZE, &above_stored, "")]).await;
    assert_eq!(
        reported_errors(&answer),
        vec![ReportedMessage {
            namespace: ASG.to_string(),
            option_name: MIN_SIZE.to_string(),
            severity: "error".to_string(),
            message: invalid_value(ASG, MIN_SIZE, &above_stored, MIN_ABOVE_MAX),
        }],
        "case \"above the stored maximum\": expected the co-requirement on the half the request \
         sent, got {}",
        answer.body
    );
}

#[tokio::test]
async fn a_write_of_an_inverted_group_size_pair_is_refused_and_stores_nothing() {
    let emulator = emulator_with_template().await;

    let mut params = vec![
        ("ApplicationName".to_string(), APPLICATION.to_string()),
        ("TemplateName".to_string(), TEMPLATE.to_string()),
    ];
    params.extend(option_params(&[
        (ASG, MIN_SIZE, "4", ""),
        (ASG, MAX_SIZE, "2", ""),
    ]));
    let refused = emulator
        .call("UpdateConfigurationTemplate", &as_params(&params))
        .await;
    assert_eq!(
        (refused.status, refused.error_code()),
        (400, "ConfigurationValidationException"),
        "case \"a write of an inverted pair\": expected HTTP 400 \
         ConfigurationValidationException, got {} and body {}",
        refused.status,
        refused.body
    );
    // The refusal states ONE of the two halves' messages: both are the same
    // co-requirement, read from each side.
    let stated = refused.error_message();
    assert!(
        [
            configuration_validation(&invalid_value(ASG, MIN_SIZE, "4", MIN_ABOVE_MAX)),
            configuration_validation(&invalid_value(ASG, MAX_SIZE, "2", MAX_BELOW_MIN)),
        ]
        .contains(&stated),
        "case \"a write of an inverted pair\": expected the configuration-validation sentence \
         carrying one half's co-requirement text, got {stated:?}"
    );
    assert_eq!(
        stored_values(&emulator, ASG, MAX_SIZE).await,
        vec![STORED_MAX_SIZE.to_string()],
        "case \"a write of an inverted pair\": expected the template's stored pair to be left as \
         it was"
    );
    assert!(
        stored_values(&emulator, ASG, MIN_SIZE)
            .await
            .iter()
            .all(|value| value != "4"),
        "case \"a write of an inverted pair\": expected the refused minimum not to be stored, \
         got {:?}",
        stored_values(&emulator, ASG, MIN_SIZE).await
    );
}
