// SPDX-License-Identifier: CC0-1.0

//! Whether a DESIRED configuration option value is one the platform version
//! accepts, and what the configuration those values result in should be pointed
//! out for: the evaluation `ValidateConfigurationSettings` reports as
//! `ValidationMessage`s, at both severities the model declares — an `error` for a
//! value the option does not take, a `warning` for information the caller should
//! take into account about a configuration whose values are each acceptable.
//!
//! A specification is resolved before it is evaluated: a `(Namespace,
//! OptionName)` pair whose namespace no platform version defines
//! ([`super::configuration_option_namespaces`]) names no configuration option,
//! so it is reported as an unknown configuration setting rather than having a
//! value of it compared with a restriction that does not exist. The same holds
//! of the option NAME inside a namespace whose names the platform itself states
//! — one the catalog below publishes options of: a name that is not among them
//! names no configuration option of that namespace either.
//!
//! The published description of an option states what values it takes — a
//! numeric range, a length, a regular expression, a value set — and
//! [`super::configuration_option_catalog`] is where this service holds those
//! restrictions, so the evaluation is made against the SAME catalog
//! `DescribeConfigurationOptions` publishes for the platform version the
//! configuration set runs: a value a caller can read the restriction for cannot
//! be accepted here and refused there, or the other way round.
//!
//! One restriction is stated in terms of ANOTHER option rather than of a
//! constant: `MinSize` is "minimum number of instances you want running. Must be
//! a positive integer no greater than MaxSize". A requirement like that is a
//! CO-REQUIREMENT between the two options rather than a bound of either one, so
//! it is resolved against the configuration the desired settings put IN FORCE
//! and reported as the co-requirement it is — which is what makes an inverted
//! pair of Auto Scaling group sizes two defects, a minimum above the maximum and
//! that maximum below the minimum, neither of them a value outside the range the
//! option's own published description states.
//!
//! A bound can also belong to a UNIT a second option chooses: `BatchSizeType`
//! says "whether BatchSize is a fixed number of instances or a percentage of
//! them", so the maximum a deployment batch size is held to is the maximum FOR
//! the batch size type in force. A message naming that maximum therefore names
//! the type it is the maximum of as well, which is what tells a caller whether
//! to lower the value or to state it in the other type.
//!
//! Split out of [`super`] so the evaluation lives beside the option tables it
//! reads, and so every operation that takes option settings has one place to
//! reach it.

use super::*;

use crate::ops::types::{ValidationMessage, VALIDATION_SEVERITY_VALUES};

/// `ValidationSeverity$error`: "this message indicates that this is not a valid
/// setting for an option". Read from the modelled value set, so this service
/// reports no severity the model does not declare.
pub const VALIDATION_SEVERITY_ERROR: &str = VALIDATION_SEVERITY_VALUES[0];

/// `ValidationSeverity$warning`: "this message is providing information you
/// should take into account". Read from the same modelled value set, so an
/// advisory this service reports carries a severity the model declares and the
/// caller can tell apart from a value it must correct.
pub const VALIDATION_SEVERITY_WARNING: &str = VALIDATION_SEVERITY_VALUES[1];

/// One `ValidationMessage` about the option `namespace` / `option_name`, at
/// `severity`.
fn reported_setting(
    severity: &str,
    namespace: &str,
    option_name: &str,
    message: String,
) -> ValidationMessage {
    ValidationMessage {
        message: Some(message),
        severity: Some(severity.to_string()),
        namespace: Some(namespace.to_string()),
        option_name: Some(option_name.to_string()),
    }
}

/// One `ValidationMessage` about the option `namespace` / `option_name`, at the
/// severity of a value that is not valid for it.
fn invalid_setting(namespace: &str, option_name: &str, message: String) -> ValidationMessage {
    reported_setting(VALIDATION_SEVERITY_ERROR, namespace, option_name, message)
}

/// The sentence a value the option does not take is reported with: the value the
/// request sent, the option it was sent for, and the requirement it failed.
fn invalid_value_message(
    namespace: &str,
    option_name: &str,
    value: &str,
    requirement: &str,
) -> String {
    format!(
        "Invalid option value: '{value}' (Namespace: '{namespace}', OptionName: \
         '{option_name}'): {requirement}"
    )
}

/// The sentence an option specification the platform cannot resolve is reported
/// with: the namespace and the option name the CALLER supplied, and the
/// statement that they name no configuration setting. FIXED text apart from
/// those two values, which are echoed as the request wrote them — a caller
/// correcting a typo'd namespace has to be told which specification it sent was
/// not resolved.
fn unknown_specification_message(namespace: &str, option_name: &str) -> String {
    format!(
        "Invalid option specification (Namespace: '{namespace}', OptionName: \
         '{option_name}'): Unknown configuration setting."
    )
}

/// The requirement a value that is not a number at all fails, on an option whose
/// published description bounds it by a numeric range: the option takes a number,
/// so the value is reported as not being one rather than as being outside a bound
/// it cannot be compared with.
const NOT_A_NUMBER_REQUIREMENT: &str = "Value is not a number";

/// The co-requirement the Auto Scaling group's minimum size breaks when the
/// maximum in force is below it: `MinSize` is "minimum number of instances you
/// want running. Must be a positive integer no greater than MaxSize", so the
/// defect is the ORDER of the pair rather than the value being outside the range
/// that option's published description states. FIXED text, naming the two
/// options the requirement relates; the value it is reported for is the one the
/// request sent.
const MIN_SIZE_ABOVE_MAX_SIZE_REQUIREMENT: &str = "MinSize is greater than MaxSize";

/// The SAME co-requirement read from the other side — the maximum size, with a
/// minimum above it in force. See [`MIN_SIZE_ABOVE_MAX_SIZE_REQUIREMENT`].
const MAX_SIZE_BELOW_MIN_SIZE_REQUIREMENT: &str = "MaxSize is less than MinSize";

/// The option that names the IAM instance profile the instances a configuration
/// launches are given — "an instance profile enables Amazon EC2 instances to
/// access other Amazon Web Services resources" — in the launch-configuration
/// namespace every platform version this service publishes defines.
///
/// Stated once here and read by [`super::environment_launch_failure`] too, so
/// the option this validation advises about and the one a launch passes a role
/// for cannot be spelled two ways.
pub(super) const OPTION_IAM_INSTANCE_PROFILE: &str = "IamInstanceProfile";

/// The advisory a configuration that associates no IAM instance profile with its
/// instances is reported with. FIXED text: it names nothing from the request or
/// from the store, and the option the advice is about is identified by the
/// message's own `Namespace` / `OptionName` instead.
const NO_INSTANCE_PROFILE_ADVISORY: &str = "The environment does not have an IAM instance profile \
                                            associated with it. To improve deployment speed \
                                            please associate an IAM instance profile with the \
                                            environment.";

/// The advisories the configuration `written` — the settings the configuration
/// set holds with the desired ones over them — carries: information the caller
/// "should take into account" about a configuration whose values are each one
/// their option takes.
///
/// One such check is stated: a configuration that sets no IAM instance profile
/// for its instances launches them without one, which is a deployment the
/// service still performs, degraded, rather than a value to correct — so it is
/// reported at the advisory severity, scoped to the option that would carry the
/// profile. A setting carrying no value, or the empty one, associates no profile,
/// the same as not setting the option at all.
fn configuration_advisory_messages(
    written: &[ConfigurationOptionSetting],
) -> Vec<ValidationMessage> {
    let associated = option_value(
        written,
        NAMESPACE_LAUNCH_CONFIGURATION,
        OPTION_IAM_INSTANCE_PROFILE,
    )
    .is_some_and(|profile| !profile.trim().is_empty());
    if associated {
        return Vec::new();
    }
    vec![reported_setting(
        VALIDATION_SEVERITY_WARNING,
        NAMESPACE_LAUNCH_CONFIGURATION,
        OPTION_IAM_INSTANCE_PROFILE,
        NO_INSTANCE_PROFILE_ADVISORY.to_string(),
    )]
}

/// What a refusal of a request that WRITES option settings opens with, ahead of
/// the validation message that says which value was not accepted:
/// `Configuration validation exception: Invalid option value: …`, the sentence
/// Elastic Beanstalk refuses a configuration it cannot accept with.
pub const CONFIGURATION_VALIDATION_PREFIX: &str = "Configuration validation exception: ";

/// The refusal a request that WRITES configuration option settings earns for a
/// desired value the option does not take — `None` where every value it wrote is
/// one the platform version's catalog accepts.
///
/// The same evaluation `ValidateConfigurationSettings` reports, so a value that
/// operation calls an error is one a write refuses, asked of the request's OWN
/// settings alone: a write is refused for a specification the platform does not
/// resolve and for a value the option itself does not take, which is what its
/// caller supplied and can correct. A co-requirement between two options is
/// resolved against those same settings, so a request that inverts the pair it
/// sends is refused for it. The first message is what the refusal states, so a
/// request carrying several defective values names the first one it sent.
pub fn configuration_validation_refusal(
    catalog: &[&'static ConfigurationOption],
    desired: &[ConfigurationOptionSetting],
) -> Option<String> {
    configuration_validation_messages(catalog, desired, desired)
        .into_iter()
        .find_map(|reported| reported.message)
        .map(|message| format!("{CONFIGURATION_VALIDATION_PREFIX}{message}"))
}

impl ComAmazonawsElasticbeanstalkAwselasticbeanstalkserviceState {
    /// The errors and the advisories the desired settings `desired` carry for the
    /// configuration set of the platform version named by `solution_stack_name` /
    /// `platform_arn`, whose own settings are `stored` — what
    /// `ValidateConfigurationSettings` answers with.
    ///
    /// The evaluation is made against that version's published catalog, and
    /// against the configuration the desired settings would put in force (see
    /// [`Self::resolved_configuration_settings`]), so a bound one option states
    /// in terms of another is resolved the way a read of the set resolves its
    /// values.
    ///
    /// Both severities the model declares are reported: one error per setting
    /// whose value its option does not take, then the advisories the resulting
    /// configuration carries (see [`configuration_advisory_messages`]) — which is
    /// what makes "any errors or warnings associated with the selection of option
    /// values" the whole answer rather than the errors alone.
    pub fn configuration_validation_messages(
        &self,
        region: &str,
        solution_stack_name: Option<&str>,
        platform_arn: Option<&str>,
        stored: &[ConfigurationOptionSetting],
        desired: &[ConfigurationOptionSetting],
    ) -> Vec<ValidationMessage> {
        let stack = configuration_platform(region, solution_stack_name, platform_arn);
        let written = merge_option_settings(stored, desired);
        let in_force = self.resolved_configuration_settings(
            region,
            solution_stack_name,
            platform_arn,
            &written,
        );
        let catalog = self.published_option_catalog(stack);
        let mut messages = configuration_validation_messages(&catalog, desired, &in_force);
        messages.extend(configuration_advisory_messages(&written));
        messages
    }

    /// The refusal a write of the option settings `written` to a configuration
    /// set of the platform version named by `solution_stack_name` /
    /// `platform_arn` earns — see [`configuration_validation_refusal`]. The one
    /// place the operations that WRITE option settings reach that evaluation,
    /// so none of them accepts a value another refuses.
    pub fn configuration_settings_refusal(
        &self,
        region: &str,
        solution_stack_name: Option<&str>,
        platform_arn: Option<&str>,
        written: &[ConfigurationOptionSetting],
    ) -> Option<String> {
        let stack = configuration_platform(region, solution_stack_name, platform_arn);
        configuration_validation_refusal(&self.published_option_catalog(stack), written)
    }
}

/// Every error the desired option settings `desired` carry against the option
/// catalog `catalog` publishes, with `in_force` the configuration those settings
/// would put in force (the platform version's defaults, the stored set, then the
/// desired values over them) — which is what a restriction stated in terms of
/// ANOTHER option is resolved against.
///
/// Each setting is RESOLVED first and evaluated second: a specification naming a
/// namespace no platform version defines, or an option name a namespace whose
/// names the platform states does not hold, names no configuration option — so
/// it is reported as an unknown configuration setting instead of having a value
/// of it evaluated (see [`super::configuration_option_namespaces`]).
///
/// One message per defective setting, in the order the request listed them, so a
/// caller sending several settings is told about each of them rather than only
/// the first; a setting whose value the option takes contributes none, which is
/// what makes an empty list the answer to a configuration that is valid.
pub fn configuration_validation_messages(
    catalog: &[&'static ConfigurationOption],
    desired: &[ConfigurationOptionSetting],
    in_force: &[ConfigurationOptionSetting],
) -> Vec<ValidationMessage> {
    let mut messages: Vec<ValidationMessage> = Vec::new();
    for setting in desired {
        let namespace = setting.namespace.as_deref().unwrap_or_default();
        let option_name = setting.option_name.as_deref().unwrap_or_default();
        // A specification is RESOLVED before any value in it is evaluated: a
        // namespace no platform version defines identifies no Amazon Web
        // Services resource, so the specification names no configuration
        // option at all and there is no restriction a value of it could be
        // compared against. That is reported as the specification being
        // unknown — the finding is about the option the caller named, not
        // about the value it carries, so it is reported whatever that value
        // is and instead of any per-value finding. See
        // `super::configuration_option_namespaces`.
        if !platform_defines_namespace(namespace) {
            messages.push(invalid_setting(
                namespace,
                option_name,
                unknown_specification_message(namespace, option_name),
            ));
            continue;
        }
        // The option NAME is resolved the same way, where the namespace STATES
        // its names: a namespace the catalog publishes options of holds the
        // platform's own option names, and the catalog
        // `DescribeConfigurationOptions` publishes for this platform version is
        // the set of them — so a name that is not among them names no
        // configuration option of that namespace, whatever value it carries.
        //
        // A namespace the catalog publishes NO option of states no names, so a
        // setting in one of those is resolved whatever it is called and carries
        // no published description to evaluate: a configuration legitimately
        // sets options whose names the CALLER chooses (the environment
        // properties passed to the application, the template parameters),
        // options scoped to a resource of their own, and options of a resource
        // this service describes none of.
        let Some(option) = catalog
            .iter()
            .find(|option| option.namespace == namespace && option.name == option_name)
        else {
            if catalog_states_option_names(catalog, namespace) {
                messages.push(invalid_setting(
                    namespace,
                    option_name,
                    unknown_specification_message(namespace, option_name),
                ));
            }
            continue;
        };
        // A setting carrying no value, or the empty one, states no desired value
        // to evaluate: it leaves the option at the value already in force rather
        // than asking for one a restriction could refuse.
        let value = setting.value.as_deref().unwrap_or_default();
        if value.is_empty() {
            continue;
        }
        if let Some(requirement) = unmet_requirement(option, value, in_force) {
            messages.push(invalid_setting(
                namespace,
                option_name,
                invalid_value_message(namespace, option_name, value, &requirement),
            ));
        }
    }
    messages
}

/// Whether the published catalog `catalog` STATES the option names of the
/// namespace `namespace` — true where it publishes a description of any option
/// in it, which is what makes the set of names it publishes for that namespace
/// the platform's own set.
///
/// A namespace whose option NAMES are the caller's own states none whatever the
/// catalog describes of it
/// ([`super::configuration_option_namespaces::namespace_names_its_own_options`]):
/// its options are one entry per value the caller chose, so the catalog's
/// entries for it are mappings a platform version ships rather than the names
/// the namespace admits.
fn catalog_states_option_names(
    catalog: &[&'static ConfigurationOption],
    namespace: &str,
) -> bool {
    !namespace_names_its_own_options(namespace)
        && catalog.iter().any(|option| option.namespace == namespace)
}

/// The requirement `value` fails of the ones `option`'s published description
/// carries — `None` where the option takes the value.
///
/// The four restrictions are the four members the description publishes: the
/// value set a selection draws from, the range a numeric option lies in, the
/// length a string option is inside, and the regular expression it matches. A
/// fifth is the co-requirement a description states in terms of ANOTHER option's
/// value, which is asked of the configuration `in_force` once the value is
/// inside the option's own published range. They are checked in that order, and
/// the FIRST one failed is reported: one setting earns one message, naming the
/// requirement it did not meet — and, where the published maximum is the maximum
/// of a unit another option chooses, the unit it belongs to as well (see
/// [`exceeds_maximum_requirement`]).
fn unmet_requirement(
    option: &ConfigurationOption,
    value: &str,
    in_force: &[ConfigurationOptionSetting],
) -> Option<String> {
    if !option.value_options.is_empty() {
        // "List: values for this option are multiple selections from the
        // possible values", stated as one comma-separated value; a `Scalar` is a
        // single selection, so its whole value is the selection.
        let selections: Vec<&str> = if option.value_type == OPTION_VALUE_TYPE_LIST {
            value.split(',').map(str::trim).collect()
        } else {
            vec![value]
        };
        if selections
            .iter()
            .any(|selection| !option.value_options.contains(selection))
        {
            return Some(format!(
                "Value is not one of the allowed values: [{}]",
                option.value_options.join(", ")
            ));
        }
    }
    // The range is the one the option's OWN published description states, so a
    // bound this reports is one `DescribeConfigurationOptions` publishes for the
    // same option — never a value read off another option.
    let minimum = option.min_value.map(i64::from);
    let maximum = option.max_value.map(i64::from);
    if minimum.is_some() || maximum.is_some() {
        // A range is published only for an option whose value IS a number, so a
        // value that is not one meets no bound of it.
        let Ok(number) = value.parse::<i64>() else {
            return Some(NOT_A_NUMBER_REQUIREMENT.to_string());
        };
        if let Some(minimum) = minimum {
            if number < minimum {
                return Some(format!("Value is below minimum allowed value: {minimum}"));
            }
        }
        if let Some(maximum) = maximum {
            if number > maximum {
                return Some(exceeds_maximum_requirement(option, maximum, in_force));
            }
        }
        if let Some(requirement) = unmet_co_requirement(option, number, in_force) {
            return Some(requirement.to_string());
        }
    }
    if let Some(max_length) = option.max_length {
        if usize::try_from(max_length).is_ok_and(|limit| value.chars().count() > limit) {
            return Some(format!("Value exceeds maximum allowed length: {max_length}"));
        }
    }
    if let Some((pattern, _)) = option.regex {
        // The published pattern is a regular expression, so it is matched by a
        // regular-expression engine rather than read by hand here. The
        // expressions are compiled ONCE, off the request path — the patterns are
        // the catalog's own static entries, so a request cannot make this
        // compile anything.
        if published_expression(pattern).is_some_and(|expression| !expression.is_match(value)) {
            return Some(format!("Value does not match the required pattern: {pattern}"));
        }
    }
    None
}

/// The requirement a number above `option`'s published maximum fails: the
/// maximum itself, and — where that maximum is the maximum of a UNIT the
/// configuration selects through another option — the unit it is the maximum
/// of, so the caller is told WHICH limit was applied rather than its number
/// alone.
fn exceeds_maximum_requirement(
    option: &ConfigurationOption,
    maximum: i64,
    in_force: &[ConfigurationOptionSetting],
) -> String {
    let exceeded = format!("Value exceeds maximum allowed value: {maximum}");
    match enforced_maximum_unit(option, in_force) {
        Some(unit) => format!("{exceeded} for {unit}"),
        None => exceeded,
    }
}

/// The clause naming the kind of number a deployment batch size is stated in,
/// as the maximum one is held to is reported with. FIXED text; the kind itself
/// is the value the configuration puts in force for it.
const BATCH_SIZE_TYPE_CLAUSE: &str = "Batch Size Type.";

/// The unit `option`'s published maximum is the maximum OF, where the option's
/// value carries a unit another option chooses — `None` for an option whose
/// value is a plain number, whose maximum is therefore reported by itself.
///
/// The one option of this catalog whose value has such a unit is the deployment
/// batch size: `BatchSizeType` states "whether BatchSize is a fixed number of
/// instances or a percentage of them", so the maximum a batch size is held to is
/// the maximum for the batch size type the configuration `in_force` puts in
/// force — the value the request sent for it, else the one the configuration set
/// already carries, else that option's documented default, which is the same
/// resolution a read of the configuration makes.
///
/// The maximum belongs to one of the types that option PUBLISHES, so a
/// configuration whose value for it is not one of them (a value that option's
/// own evaluation reports as an error of its own) leaves the published default
/// the maximum in force belongs to.
fn enforced_maximum_unit(
    option: &ConfigurationOption,
    in_force: &[ConfigurationOptionSetting],
) -> Option<String> {
    if option.namespace != NAMESPACE_COMMAND || option.name != OPTION_BATCH_SIZE {
        return None;
    }
    let batch_size_type = option_value(in_force, NAMESPACE_COMMAND, OPTION_BATCH_SIZE_TYPE)
        .map(str::trim)
        .filter(|batch_size_type| BATCH_SIZE_TYPES.contains(batch_size_type))
        .unwrap_or(BATCH_SIZE_TYPE_PERCENTAGE);
    Some(format!("{batch_size_type} {BATCH_SIZE_TYPE_CLAUSE}"))
}

/// The compiled form of the published pattern `pattern`, from the expressions of
/// every option this service's catalog can publish, compiled on first use.
///
/// Compiling belongs here rather than in the evaluation: the patterns are static
/// table entries ([`super::configuration_option_catalog`]), and one request can
/// carry hundreds of settings, so the expressions are built once for the process
/// and reused. A pattern this engine cannot compile has no expression and so
/// restricts nothing, which refuses no value the real service accepts.
fn published_expression(pattern: &'static str) -> Option<&'static ::regex::Regex> {
    static EXPRESSIONS: ::std::sync::OnceLock<Vec<(&'static str, ::regex::Regex)>> =
        ::std::sync::OnceLock::new();
    EXPRESSIONS
        .get_or_init(|| {
            every_publishable_option()
                .filter_map(|option| option.regex)
                .filter_map(|(pattern, _)| {
                    ::regex::Regex::new(pattern).ok().map(|compiled| (pattern, compiled))
                })
                .collect()
        })
        .iter()
        .find(|(published, _)| *published == pattern)
        .map(|(_, compiled)| compiled)
}

/// The co-requirement the desired `value` of `option` breaks against the
/// configuration `in_force` — `None` where the option's published description
/// states none, or where the configuration in force keeps it.
///
/// The one such requirement this catalog publishes is the Auto Scaling group's:
/// `MinSize` is "minimum number of instances you want running. Must be a
/// positive integer no greater than MaxSize", which relates the two sizes rather
/// than bounding either one — so a minimum above the maximum in force breaks it,
/// and so does a maximum below the minimum in force, the same requirement read
/// from the other side. That is what makes an inverted pair two defects, one on
/// each half, each naming the requirement the pair broke instead of a limit the
/// option does not publish.
///
/// A configuration that puts no number in force for the other half states no
/// such requirement (a half set to something that is not a number fails its own
/// option's published range, which is where that is reported).
fn unmet_co_requirement(
    option: &ConfigurationOption,
    value: i64,
    in_force: &[ConfigurationOptionSetting],
) -> Option<&'static str> {
    if option.namespace != NAMESPACE_AUTOSCALING_ASG {
        return None;
    }
    if option.name == OPTION_MIN_SIZE {
        let maximum = group_size(in_force, OPTION_MAX_SIZE)?;
        (value > maximum).then_some(MIN_SIZE_ABOVE_MAX_SIZE_REQUIREMENT)
    } else if option.name == OPTION_MAX_SIZE {
        let minimum = group_size(in_force, OPTION_MIN_SIZE)?;
        (value < minimum).then_some(MAX_SIZE_BELOW_MIN_SIZE_REQUIREMENT)
    } else {
        None
    }
}

/// The number the Auto Scaling group bound `option_name` is set to in
/// `settings`, where it is set to a number at all.
fn group_size(settings: &[ConfigurationOptionSetting], option_name: &str) -> Option<i64> {
    option_value(settings, NAMESPACE_AUTOSCALING_ASG, option_name)?
        .parse::<i64>()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One option setting, as a request sends it.
    fn setting(namespace: &str, option_name: &str, value: &str) -> ConfigurationOptionSetting {
        ConfigurationOptionSetting {
            resource_name: None,
            namespace: Some(namespace.to_string()),
            option_name: Some(option_name.to_string()),
            value: Some(value.to_string()),
        }
    }

    /// The catalog every platform version defines, which is what the published
    /// restrictions below are read from.
    fn catalog() -> Vec<&'static ConfigurationOption> {
        published_configuration_options(None, false)
    }

    /// The `(OptionName, Message)` pairs an evaluation reported.
    fn reported(messages: &[ValidationMessage]) -> Vec<(String, String)> {
        messages
            .iter()
            .map(|message| {
                (
                    message.option_name.clone().unwrap_or_default(),
                    message.message.clone().unwrap_or_default(),
                )
            })
            .collect()
    }

    /// RULE: a desired value is evaluated against the restriction the published
    /// description of its own option carries, so one inside it earns no message.
    #[test]
    fn a_value_inside_the_published_restriction_is_accepted() {
        let desired = vec![setting(NAMESPACE_AUTOSCALING_ASG, OPTION_MIN_SIZE, "2")];
        let in_force = vec![setting(NAMESPACE_AUTOSCALING_ASG, OPTION_MAX_SIZE, "4")];
        let messages = configuration_validation_messages(&catalog(), &desired, &in_force);
        assert!(
            messages.is_empty(),
            "case \"inside the published range\": expected no message, got {messages:?}"
        );
    }

    /// RULE: a specification whose namespace no platform version defines names
    /// no configuration option, so it earns one error-severity message saying
    /// the setting is unknown — whatever value it carries, and instead of any
    /// finding about that value.
    #[test]
    fn a_specification_in_a_namespace_no_platform_defines_is_reported_as_unknown() {
        // The option NAME is one the catalog does publish, in another namespace,
        // so what is reported is about the namespace rather than the name; the
        // value is one that namespace's option would also refuse, so a second
        // message would show the evaluation had gone on to the value.
        let undefined = "aws:elasticbeanstalk:namespace-no-platform-defines";
        for value in ["2", "not-a-number", ""] {
            let desired = vec![setting(undefined, OPTION_MIN_SIZE, value)];
            let messages = configuration_validation_messages(&catalog(), &desired, &desired);
            assert_eq!(
                reported(&messages),
                vec![(
                    OPTION_MIN_SIZE.to_string(),
                    format!(
                        "Invalid option specification (Namespace: '{undefined}', OptionName: \
                         '{OPTION_MIN_SIZE}'): Unknown configuration setting."
                    )
                )],
                "case {value:?}: expected one message about the unknown specification, got \
                 {messages:?}"
            );
            assert_eq!(
                messages[0].severity.as_deref(),
                Some(VALIDATION_SEVERITY_ERROR),
                "case {value:?}: expected the error severity, got {:?}",
                messages[0]
            );
        }
    }

    /// RULE: a value outside the published range earns one message naming that
    /// option, the value sent and the bound it exceeded.
    #[test]
    fn a_value_outside_the_published_range_is_reported() {
        let above = (i64::from(MAX_PUBLISHED_ASG_SIZE) + 1).to_string();
        let desired = vec![setting(NAMESPACE_AUTOSCALING_ASG, OPTION_MAX_SIZE, &above)];
        let messages = configuration_validation_messages(&catalog(), &desired, &desired);
        assert_eq!(
            reported(&messages),
            vec![(
                OPTION_MAX_SIZE.to_string(),
                format!(
                    "Invalid option value: '{above}' (Namespace: '{NAMESPACE_AUTOSCALING_ASG}', \
                     OptionName: '{OPTION_MAX_SIZE}'): Value exceeds maximum allowed value: \
                     {MAX_PUBLISHED_ASG_SIZE}"
                )
            )],
            "case \"above the published maximum\": expected the published bound in one message, \
             got {messages:?}"
        );
        assert_eq!(
            messages[0].severity.as_deref(),
            Some(VALIDATION_SEVERITY_ERROR),
            "case \"above the published maximum\": expected the error severity, got {:?}",
            messages[0]
        );
    }

    /// RULE: the group's minimum may be no greater than its maximum, so an
    /// inverted pair is reported on BOTH halves — each naming the
    /// co-requirement the pair broke rather than a limit of the option itself,
    /// whose published range both values are inside.
    #[test]
    fn an_inverted_group_size_pair_is_reported_on_both_halves() {
        let desired = vec![
            setting(NAMESPACE_AUTOSCALING_ASG, OPTION_MIN_SIZE, "4"),
            setting(NAMESPACE_AUTOSCALING_ASG, OPTION_MAX_SIZE, "2"),
        ];
        let messages = configuration_validation_messages(&catalog(), &desired, &desired);
        assert_eq!(
            reported(&messages),
            vec![
                (
                    OPTION_MIN_SIZE.to_string(),
                    format!(
                        "Invalid option value: '4' (Namespace: '{NAMESPACE_AUTOSCALING_ASG}', \
                         OptionName: '{OPTION_MIN_SIZE}'): \
                         {MIN_SIZE_ABOVE_MAX_SIZE_REQUIREMENT}"
                    )
                ),
                (
                    OPTION_MAX_SIZE.to_string(),
                    format!(
                        "Invalid option value: '2' (Namespace: '{NAMESPACE_AUTOSCALING_ASG}', \
                         OptionName: '{OPTION_MAX_SIZE}'): \
                         {MAX_SIZE_BELOW_MIN_SIZE_REQUIREMENT}"
                    )
                ),
            ],
            "case \"an inverted pair\": expected a message on each half, got {messages:?}"
        );
    }

    /// RULE: an option name a namespace whose names the catalog states does not
    /// hold names no configuration option, so it earns the same unknown-setting
    /// message an unresolved namespace does; a namespace the catalog states no
    /// names of resolves whatever the option is called.
    #[test]
    fn an_option_name_the_namespace_does_not_hold_is_reported_as_unknown() {
        let unknown = "NotAnOptionInThisNamespace";
        let desired = vec![
            setting(NAMESPACE_AUTOSCALING_ASG, unknown, "2"),
            setting(NAMESPACE_APPLICATION_ENVIRONMENT, unknown, "2"),
        ];
        let messages = configuration_validation_messages(&catalog(), &desired, &desired);
        assert_eq!(
            reported(&messages),
            vec![(
                unknown.to_string(),
                format!(
                    "Invalid option specification (Namespace: '{NAMESPACE_AUTOSCALING_ASG}', \
                     OptionName: '{unknown}'): Unknown configuration setting."
                )
            )],
            "case \"a name the namespace does not publish\": expected one message, about the \
             namespace whose names the catalog states, got {messages:?}"
        );
    }

    /// RULE: every pattern the catalog publishes is a regular expression this
    /// engine can compile, so no published restriction silently stops
    /// restricting.
    #[test]
    fn every_published_pattern_compiles() {
        for option in every_publishable_option() {
            if let Some((pattern, _)) = option.regex {
                assert!(
                    published_expression(pattern).is_some(),
                    "case {:?}: expected the published pattern {pattern:?} to compile",
                    option.key()
                );
            }
        }
    }
}
