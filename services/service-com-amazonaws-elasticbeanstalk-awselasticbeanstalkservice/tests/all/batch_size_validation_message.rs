// SPDX-License-Identifier: CC0-1.0

//! The validation message a deployment batch size above the maximum it is held
//! to is reported with, driven end to end over the service's awsQuery HTTP
//! surface — through `ValidateConfigurationSettings`, which reports it, and
//! through the operations that WRITE option settings, which refuse for it.
//!
//! The rule under test: an option whose value carries a UNIT another option
//! chooses is held to the maximum FOR that unit, so the message naming that
//! maximum names the unit it is the maximum of as well. The deployment batch
//! size is the option of this kind: `BatchSizeType` states "whether BatchSize is
//! a fixed number of instances or a percentage of them", so a batch size above
//! the published maximum is reported "Value exceeds maximum allowed value:
//! <maximum> for <BatchSizeType> Batch Size Type." — the maximum the catalogue
//! publishes for the option, and the batch size type the configuration puts in
//! force. A numeric option whose value is a plain number is reported with its
//! maximum alone, and a batch size type the option publishes no description of
//! leaves the maximum in force the one the published default belongs to.
//!
//! Rows differ along the batch size type in force and along WHERE it comes from
//! — the type the same request sends, the type the configuration set the
//! evaluation is made for already stores, and the type's documented default when
//! nothing sets it — along the operation the settings reach the evaluation
//! through, and along where the value sits relative to each published bound. The
//! bounds and the type's own value set are read from the catalogue the service
//! publishes (`eb::state::published_configuration_options`, what
//! `DescribeConfigurationOptions` reports), so no row states a restriction this
//! service does not publish.
//!
//! The HTTP harness (`Emulator`, `Answer`) is the one `all/lifecycle.rs` owns in
//! this same test binary, the option-setting encoder the one
//! `all/configuration_templates.rs` owns, and the `ValidationMessage` reader and
//! the catalogue lookup the ones `all/configuration_setting_validation.rs` owns,
//! so this suite keeps no second copy of any of them.

use crate::configuration_setting_validation::{
    invalid_value, published, reported_errors, ReportedMessage,
};
use crate::configuration_templates::{as_params, created, option_params, published_stack};
use crate::harness::{wait_for_status, Answer, Emulator};
use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;

/// The application every configuration set below belongs to.
const APPLICATION: &str = "batch-size-app";
/// A stored configuration template that sets no batch size type, so the type's
/// documented default is the one in force for it.
const TEMPLATE: &str = "batch-size-template";
/// A second stored template that carries a batch size type of its own, so a row
/// can show the type being read from the configuration SET rather than from the
/// request or from the default.
const TYPED_TEMPLATE: &str = "batch-size-typed-template";
/// The running environment the write row updates.
const ENVIRONMENT: &str = "batch-size-env";

/// The namespace of the deployment command options, and the two options in it
/// that decide how large one deployment batch is, as the service's own catalogue
/// names them.
const COMMAND: &str = "aws:elasticbeanstalk:command";
/// See [`COMMAND`].
const BATCH_SIZE: &str = "BatchSize";
/// See [`COMMAND`].
const BATCH_SIZE_TYPE: &str = "BatchSizeType";
/// A third option of that same namespace, whose value is a plain number of
/// seconds with no unit another option chooses — the contrast row.
const TIMEOUT: &str = "Timeout";

/// The batch size type of a size stated as a share of the environment's
/// instances, which the catalogue publishes as the option's default, and the one
/// of a size stated as a count of them. Both are asserted to be in the value set
/// the catalogue publishes for the option before they are used.
const PERCENTAGE: &str = "Percentage";
/// See [`PERCENTAGE`].
const FIXED: &str = "Fixed";

/// One batch size type as the clause of a validation message states it.
fn batch_size_type_clause(batch_size_type: &str) -> String {
    format!("{batch_size_type} Batch Size Type.")
}

/// The requirement a batch size above the maximum for `batch_size_type` fails,
/// as the message must state it: the published maximum, then the type that
/// maximum is the maximum of.
fn exceeds_maximum(maximum: i32, batch_size_type: &str) -> String {
    format!(
        "Value exceeds maximum allowed value: {maximum} for {}",
        batch_size_type_clause(batch_size_type)
    )
}

/// The published `MaxValue` of `namespace` / `option_name` on the platform
/// version the fixtures run.
fn published_maximum(namespace: &str, option_name: &str) -> i32 {
    published(namespace, option_name)
        .max_value
        .unwrap_or_else(|| panic!("the catalogue publishes a MaxValue for {option_name}"))
}

/// The published `MinValue` of `namespace` / `option_name` on that same platform
/// version.
fn published_minimum(namespace: &str, option_name: &str) -> i32 {
    published(namespace, option_name)
        .min_value
        .unwrap_or_else(|| panic!("the catalogue publishes a MinValue for {option_name}"))
}

/// An emulator holding the application, the two templates and the running
/// environment the rows are evaluated against.
async fn emulator_with_configuration_sets() -> Emulator {
    let emulator = Emulator::new();
    created(&emulator, "CreateApplication", &[("ApplicationName", APPLICATION)]).await;
    let stack = published_stack(0).name();
    for (template, settings) in [
        (TEMPLATE, Vec::new()),
        (
            TYPED_TEMPLATE,
            option_params(&[(COMMAND, BATCH_SIZE_TYPE, FIXED, "")]),
        ),
    ] {
        let mut params = vec![
            ("ApplicationName".to_string(), APPLICATION.to_string()),
            ("TemplateName".to_string(), template.to_string()),
            ("SolutionStackName".to_string(), stack.clone()),
        ];
        params.extend(settings);
        created(
            &emulator,
            "CreateConfigurationTemplate",
            &as_params(&params),
        )
        .await;
    }
    created(
        &emulator,
        "CreateEnvironment",
        &[
            ("ApplicationName", APPLICATION),
            ("EnvironmentName", ENVIRONMENT),
            ("SolutionStackName", &stack),
        ],
    )
    .await;
    emulator
}

/// Validate `settings` against the stored template `template`, requiring a 200:
/// the evaluation ANSWERS, and the messages are what it answers with.
async fn validated(
    emulator: &Emulator,
    template: &str,
    settings: &[(&str, &str, &str, &str)],
) -> Answer {
    let mut params = vec![
        ("ApplicationName".to_string(), APPLICATION.to_string()),
        ("TemplateName".to_string(), template.to_string()),
    ];
    params.extend(option_params(settings));
    created(
        emulator,
        "ValidateConfigurationSettings",
        &as_params(&params),
    )
    .await
}

/// One row of the message table: the configuration set the evaluation is made
/// for, the settings the request sends, and the one error the answer must report
/// for them — `None` where every value is one its option takes.
struct MessageCase {
    case: &'static str,
    template: &'static str,
    settings: Vec<(&'static str, &'static str, String, &'static str)>,
    /// The option the error must be reported for, and the requirement it must
    /// state.
    reported: Option<(&'static str, String, String)>,
}

#[tokio::test]
async fn a_batch_size_above_its_maximum_is_reported_with_the_type_that_maximum_belongs_to() {
    let emulator = emulator_with_configuration_sets().await;

    // The two batch size types the rows use are the ones the catalogue publishes
    // for the option, so no row names a type this service does not accept.
    let batch_size_type = published(COMMAND, BATCH_SIZE_TYPE);
    for named in [PERCENTAGE, FIXED] {
        assert!(
            batch_size_type.value_options.contains(&named),
            "case {named:?}: expected the catalogue to publish the batch size type, it publishes \
             {:?}",
            batch_size_type.value_options
        );
    }

    // The bounds come from the published description of each option, so a row
    // states no limit this service does not report through
    // `DescribeConfigurationOptions`.
    let maximum = published_maximum(COMMAND, BATCH_SIZE);
    let minimum = published_minimum(COMMAND, BATCH_SIZE);
    let timeout_maximum = published_maximum(COMMAND, TIMEOUT);
    let above = (i64::from(maximum) + 1).to_string();
    let far_above = (i64::from(maximum) * 20).to_string();
    let above_timeout = (i64::from(timeout_maximum) + 1).to_string();

    let cases = vec![
        MessageCase {
            case: "the batch size type the request itself sends",
            template: TEMPLATE,
            settings: vec![
                (COMMAND, BATCH_SIZE_TYPE, PERCENTAGE.to_string(), ""),
                (COMMAND, BATCH_SIZE, above.clone(), ""),
            ],
            reported: Some((
                BATCH_SIZE,
                above.clone(),
                exceeds_maximum(maximum, PERCENTAGE),
            )),
        },
        MessageCase {
            case: "the other batch size type, sent by the same request",
            template: TEMPLATE,
            settings: vec![
                (COMMAND, BATCH_SIZE_TYPE, FIXED.to_string(), ""),
                (COMMAND, BATCH_SIZE, far_above.clone(), ""),
            ],
            reported: Some((
                BATCH_SIZE,
                far_above.clone(),
                exceeds_maximum(maximum, FIXED),
            )),
        },
        MessageCase {
            case: "no batch size type anywhere, so the published default is in force",
            template: TEMPLATE,
            settings: vec![(COMMAND, BATCH_SIZE, above.clone(), "")],
            reported: Some((
                BATCH_SIZE,
                above.clone(),
                exceeds_maximum(maximum, PERCENTAGE),
            )),
        },
        MessageCase {
            case: "the batch size type the configuration set already stores",
            template: TYPED_TEMPLATE,
            settings: vec![(COMMAND, BATCH_SIZE, above.clone(), "")],
            reported: Some((BATCH_SIZE, above.clone(), exceeds_maximum(maximum, FIXED))),
        },
        MessageCase {
            case: "the request's type over the one the configuration set stores",
            template: TYPED_TEMPLATE,
            settings: vec![
                (COMMAND, BATCH_SIZE_TYPE, PERCENTAGE.to_string(), ""),
                (COMMAND, BATCH_SIZE, above.clone(), ""),
            ],
            reported: Some((
                BATCH_SIZE,
                above.clone(),
                exceeds_maximum(maximum, PERCENTAGE),
            )),
        },
        MessageCase {
            case: "a batch size at the published maximum",
            template: TEMPLATE,
            settings: vec![(COMMAND, BATCH_SIZE, maximum.to_string(), "")],
            reported: None,
        },
        MessageCase {
            case: "a batch size at the published minimum",
            template: TYPED_TEMPLATE,
            settings: vec![(COMMAND, BATCH_SIZE, minimum.to_string(), "")],
            reported: None,
        },
        MessageCase {
            case: "a batch size one below the published minimum",
            template: TEMPLATE,
            settings: vec![(
                COMMAND,
                BATCH_SIZE,
                (i64::from(minimum) - 1).to_string(),
                "",
            )],
            reported: Some((
                BATCH_SIZE,
                (i64::from(minimum) - 1).to_string(),
                format!("Value is below minimum allowed value: {minimum}"),
            )),
        },
        MessageCase {
            case: "an option of the same namespace whose value carries no such type",
            template: TEMPLATE,
            settings: vec![(COMMAND, TIMEOUT, above_timeout.clone(), "")],
            reported: Some((
                TIMEOUT,
                above_timeout.clone(),
                format!("Value exceeds maximum allowed value: {timeout_maximum}"),
            )),
        },
    ];

    for case in &cases {
        let settings: Vec<(&str, &str, &str, &str)> = case
            .settings
            .iter()
            .map(|(namespace, option_name, value, resource_name)| {
                (*namespace, *option_name, value.as_str(), *resource_name)
            })
            .collect();
        let answer = validated(&emulator, case.template, &settings).await;
        let reported = reported_errors(&answer);
        match case.reported.as_ref() {
            None => assert!(
                reported.is_empty(),
                "case {:?}: expected no error-severity validation message, got {reported:?}",
                case.case
            ),
            Some((option_name, value, requirement)) => assert_eq!(
                reported,
                vec![ReportedMessage {
                    namespace: COMMAND.to_string(),
                    option_name: (*option_name).to_string(),
                    severity: "error".to_string(),
                    message: invalid_value(COMMAND, option_name, value, requirement),
                }],
                "case {:?}: expected one error naming the option, the value and the limit it \
                 failed, got {reported:?}",
                case.case
            ),
        }
    }
}

#[tokio::test]
async fn a_batch_size_type_the_option_does_not_publish_leaves_its_published_default_in_the_clause()
{
    let emulator = emulator_with_configuration_sets().await;
    let batch_size_type = published(COMMAND, BATCH_SIZE_TYPE);
    let maximum = published_maximum(COMMAND, BATCH_SIZE);
    let above = (i64::from(maximum) + 1).to_string();
    // A batch size type the option publishes no description of is a value that
    // option itself does not take, so the maximum in force is still the one the
    // published default belongs to: the clause names a type this service
    // publishes rather than whatever the request wrote.
    let unpublished = "Share";
    assert!(
        !batch_size_type.value_options.contains(&unpublished),
        "case {unpublished:?}: expected the catalogue NOT to publish the batch size type, it \
         publishes {:?}",
        batch_size_type.value_options
    );

    let answer = validated(
        &emulator,
        TYPED_TEMPLATE,
        &[
            (COMMAND, BATCH_SIZE_TYPE, unpublished, ""),
            (COMMAND, BATCH_SIZE, &above, ""),
        ],
    )
    .await;
    let reported = reported_errors(&answer);
    assert_eq!(
        reported,
        vec![
            ReportedMessage {
                namespace: COMMAND.to_string(),
                option_name: BATCH_SIZE_TYPE.to_string(),
                severity: "error".to_string(),
                message: invalid_value(
                    COMMAND,
                    BATCH_SIZE_TYPE,
                    unpublished,
                    &format!(
                        "Value is not one of the allowed values: [{}]",
                        batch_size_type.value_options.join(", ")
                    )
                ),
            },
            ReportedMessage {
                namespace: COMMAND.to_string(),
                option_name: BATCH_SIZE.to_string(),
                severity: "error".to_string(),
                message: invalid_value(
                    COMMAND,
                    BATCH_SIZE,
                    &above,
                    &exceeds_maximum(maximum, PERCENTAGE)
                ),
            },
        ],
        "case \"a batch size type the option does not publish\": expected the type reported \
         against its own value set and the size against the published default's maximum, got \
         {reported:?}"
    );
}

/// One writing operation, and the request that writes the batch size through it.
struct WriteCase {
    case: &'static str,
    action: &'static str,
    /// The request members beyond the option settings.
    named: Vec<(String, String)>,
    /// The batch size type the request sends, where it sends one.
    batch_size_type: Option<&'static str>,
    /// The type the clause of the refusal must then name.
    clause: &'static str,
}

#[tokio::test]
async fn a_write_of_a_batch_size_above_its_maximum_is_refused_with_that_same_clause() {
    let emulator = emulator_with_configuration_sets().await;
    let stack = published_stack(0).name();
    // An environment takes an update only once the launch the clock owes it has
    // completed, so the row that writes through `UpdateEnvironment` waits for the
    // CONDITION that it reports itself ready.
    wait_for_status(
        &emulator,
        "a write to a running environment",
        ENVIRONMENT,
        "Ready",
    )
    .await;
    let maximum = published_maximum(COMMAND, BATCH_SIZE);
    let above = (i64::from(maximum) + 1).to_string();

    // Rows are the operations that WRITE option settings, each with a different
    // batch size type in force: the clause is the evaluation's, not any one
    // handler's.
    let cases = vec![
        WriteCase {
            case: "UpdateEnvironment, with the type the request sends",
            action: "UpdateEnvironment",
            named: vec![
                ("ApplicationName".to_string(), APPLICATION.to_string()),
                ("EnvironmentName".to_string(), ENVIRONMENT.to_string()),
            ],
            batch_size_type: Some(FIXED),
            clause: FIXED,
        },
        WriteCase {
            case: "UpdateConfigurationTemplate, with no type sent",
            action: "UpdateConfigurationTemplate",
            named: vec![
                ("ApplicationName".to_string(), APPLICATION.to_string()),
                ("TemplateName".to_string(), TEMPLATE.to_string()),
            ],
            batch_size_type: None,
            clause: PERCENTAGE,
        },
        WriteCase {
            case: "CreateConfigurationTemplate, with the share type sent",
            action: "CreateConfigurationTemplate",
            named: vec![
                ("ApplicationName".to_string(), APPLICATION.to_string()),
                ("TemplateName".to_string(), "batch-size-refused".to_string()),
                ("SolutionStackName".to_string(), stack.clone()),
            ],
            batch_size_type: Some(PERCENTAGE),
            clause: PERCENTAGE,
        },
        WriteCase {
            case: "CreateEnvironment, with no type sent",
            action: "CreateEnvironment",
            named: vec![
                ("ApplicationName".to_string(), APPLICATION.to_string()),
                ("EnvironmentName".to_string(), "batch-size-refused".to_string()),
                ("SolutionStackName".to_string(), stack.clone()),
            ],
            batch_size_type: None,
            clause: PERCENTAGE,
        },
    ];

    for case in &cases {
        let mut settings: Vec<(&str, &str, &str, &str)> = Vec::new();
        if let Some(batch_size_type) = case.batch_size_type {
            settings.push((COMMAND, BATCH_SIZE_TYPE, batch_size_type, ""));
        }
        settings.push((COMMAND, BATCH_SIZE, &above, ""));
        let mut params = case.named.clone();
        params.extend(option_params(&settings));
        let refused = emulator.call(case.action, &as_params(&params)).await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, "ConfigurationValidationException"),
            "case {:?}: expected HTTP 400 ConfigurationValidationException, got {} and body {}",
            case.case,
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.error_message(),
            format!(
                "{}{}",
                eb::state::CONFIGURATION_VALIDATION_PREFIX,
                invalid_value(
                    COMMAND,
                    BATCH_SIZE,
                    &above,
                    &exceeds_maximum(maximum, case.clause)
                )
            ),
            "case {:?}: expected the refusal to carry the validation message, clause included, \
             got {}",
            case.case,
            refused.body
        );
    }

    // The refused writes stored nothing: the template they named does not exist,
    // and the environment the update was refused for still runs the batch size it
    // ran.
    let applications = created(
        &emulator,
        "DescribeApplications",
        &[("ApplicationNames.member.1", APPLICATION)],
    )
    .await;
    let mut templates = applications.values_at("ConfigurationTemplates/member");
    templates.sort_unstable();
    assert_eq!(
        templates,
        vec![TEMPLATE, TYPED_TEMPLATE],
        "case \"after the refusals\": expected the refused create to store no template, got {}",
        applications.body
    );
    let settings = created(
        &emulator,
        "DescribeConfigurationSettings",
        &[
            ("ApplicationName", APPLICATION),
            ("EnvironmentName", ENVIRONMENT),
        ],
    )
    .await;
    let stored_batch_size: Vec<String> = settings
        .elements_at("OptionSettings/member")
        .iter()
        .filter(|member| {
            member.value("Namespace") == Some(COMMAND)
                && member.value("OptionName") == Some(BATCH_SIZE)
        })
        .filter_map(|member| member.value("Value").map(str::to_string))
        .collect();
    assert!(
        !stored_batch_size.contains(&above),
        "case \"after the refusals\": expected the refused batch size not to be stored, got \
         {stored_batch_size:?} in {}",
        settings.body
    );
}
