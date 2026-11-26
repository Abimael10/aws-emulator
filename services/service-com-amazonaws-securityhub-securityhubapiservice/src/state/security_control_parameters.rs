// SPDX-License-Identifier: CC0-1.0

//! What one account has CUSTOMIZED about a published security control, and how a
//! control is reported once that customization is merged over the published catalog.
//!
//! A security control's DEFINITION is service metadata, identical for every caller
//! ([`super::security_controls`]); what a caller can change about a control is its
//! PARAMETERS, and `UpdateSecurityControl` changes them "for the current Amazon Web
//! Services account and Amazon Web Services Region"
//! (`smithy_query(query="doc",shape="SecurityControl")` and the operation's own docs).
//! So a customization is keyed on the hub plus the `SecurityControlId`, and
//! `BatchGetSecurityControls` reports a control's EFFECTIVE parameters: the values this
//! account set, over the defaults the catalog publishes for every parameter it did not.
//!
//! Only a value the catalog publishes a parameter for can be stored, and only in the
//! type that parameter's `ConfigurationOptions` names, so this module is also where a
//! custom value is checked against the published options.

use std::collections::HashMap;

use super::*;
use crate::ops::types::{ParameterValue, SecurityControl};

/// `ParameterValueType$CUSTOM`: the parameter "uses a custom user-defined value", and
/// then "the `Value` field can't be empty" (`smithy_query(query="doc",
/// shape="ParameterConfiguration", member="ValueType")`).
pub const PARAMETER_VALUE_TYPE_CUSTOM: &str = "CUSTOM";

/// `ParameterValueType$DEFAULT`: the parameter "subscribes to the default Security Hub
/// CSPM behavior", and "Security Hub CSPM ignores user-provided input for the `Value`
/// field" — so a parameter reported this way carries no value of its own.
pub const PARAMETER_VALUE_TYPE_DEFAULT: &str = "DEFAULT";

/// `UpdateStatus$READY`: "Security Hub CSPM uses the current control parameter values
/// when running security checks of the control" (`smithy_query(query="doc",
/// shape="SecurityControl", member="UpdateStatus")`). A write to this store takes
/// effect at once — the very next read of the control already answers with the values
/// it set — so a control here always runs its checks with the parameters it reports.
pub const UPDATE_STATUS_READY: &str = "READY";

/// The `security-control/<SecurityControlId>` resource of a `SecurityControlArn`.
pub const SECURITY_CONTROL_RESOURCE_PREFIX: &str = "security-control/";

/// How many controls one `BatchGetSecurityControls` call may address. The model bounds
/// `StringList` nowhere and AWS publishes no quota for this list, so the bound is this
/// crate's own, the batch bound it already applies to a caller-sized list of resources
/// ([`MAX_MEMBER_BATCH`] doubled, as `BatchGetSecurityControls` addresses published
/// metadata rather than per-account resources): the work one call does under the state
/// lock, and the response it produces, must not grow with what a caller sends. A longer
/// batch is refused as the invalid input it is, which is what the operation models
/// `InvalidInputException` for — "an invalid or out-of-range value for an input
/// parameter". Since AWS publishes no quota here, that refusal is the emulator's own:
/// a caller addressing more controls in one call than this bound allows is refused
/// where AWS might answer, which is the price of keeping one call bounded (the catalog
/// publishes far fewer controls than the bound, so only a repeated identifier reaches
/// it).
pub const MAX_SECURITY_CONTROL_BATCH: usize = MAX_MEMBER_BATCH * 2;

/// The longest `LastUpdateReason` this store keeps. The model bounds neither
/// `AlphaNumericNonEmptyString` nor that member, so the cap is the one this crate
/// already applies to an unbounded string member
/// ([`MAX_ACTION_TARGET_DESCRIPTION_LEN`]): one record's size must not grow with
/// whatever a caller sends.
pub const MAX_LAST_UPDATE_REASON_LEN: usize = MAX_ACTION_TARGET_DESCRIPTION_LEN;

/// A customization's identity: the hub it belongs to, plus the id of the published
/// control it customizes. One record per control per account/Region, which is what
/// makes a second `UpdateSecurityControl` of the same control change the same record
/// rather than add a second one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SecurityControlCustomizationKey {
    pub account_id: String,
    pub region: String,
    pub security_control_id: String,
}

/// What `UpdateSecurityControl` persisted for one control of one hub.
#[derive(Debug, Clone, Default)]
pub struct SecurityControlCustomization {
    /// The parameters this account set a CUSTOM value for, keyed by the parameter name
    /// the catalog publishes. A parameter absent here uses the published default, so
    /// the map holds customizations only — and restoring a parameter to `DEFAULT`
    /// removes its entry rather than storing a copy of the published value.
    pub parameters: HashMap<String, ParameterValue>,
    /// `SecurityControl$LastUpdateReason`: "The most recent reason for updating the
    /// customizable properties of a security control", as the most recent update
    /// carried it — absent when that update named none.
    pub last_update_reason: Option<String>,
}

/// The key of the customization of `security_control_id` for hub `hub` — the single
/// definition of how a control-parameter operation addresses the record it reads or
/// writes.
pub fn security_control_customization_key(
    hub: &HubKey,
    security_control_id: &str,
) -> SecurityControlCustomizationKey {
    SecurityControlCustomizationKey {
        account_id: hub.0.clone(),
        region: hub.1.clone(),
        security_control_id: security_control_id.to_string(),
    }
}

/// `arn:aws:securityhub:<region>:<account>:security-control/<SecurityControlId>`: the
/// ARN a security control is reported under. A control is standard-agnostic, so its
/// ARN names no standard — "The security control ID or Amazon Resource Name (ARN) is
/// the same across standards" (`smithy_query(query="doc",
/// shape="BatchGetSecurityControlsRequest", member="SecurityControlIds")`).
pub fn security_control_arn(region: &str, security_control_id: &str) -> String {
    ::engine::emulator_arn(
        ARN_NAMESPACE,
        region,
        &format!("{SECURITY_CONTROL_RESOURCE_PREFIX}{security_control_id}"),
    )
}

/// The published control an operation that accepts "SecurityControlId,
/// SecurityControlArn, or a mix of both parameters" is addressing, or `None` when the
/// catalog publishes no control under it. The ARN form is resolved by deriving the ARN
/// of each published control the one way [`security_control_arn`] does, so an operation
/// never parses an ARN of its own.
pub fn security_control_by_id_or_arn(
    region: &str,
    identifier: &str,
) -> Option<&'static SecurityControlCatalogEntry> {
    security_control_by_id(identifier).or_else(|| {
        SECURITY_CONTROL_CATALOG
            .iter()
            .find(|entry| security_control_arn(region, entry.control.control_id) == identifier)
    })
}

/// The parameter `entry` publishes under `name`, or `None` when the control publishes
/// none — which is what makes an unknown parameter name, and every parameter name at
/// all on a control that supports none, refusable.
pub fn published_control_parameter(
    entry: &SecurityControlCatalogEntry,
    name: &str,
) -> Option<&'static PublishedControlParameter> {
    entry.parameters.iter().find(|parameter| parameter.name == name)
}

/// The `ParameterValue` union member a value carries, as the model names it — the word
/// a refusal uses to say what a request sent.
pub fn parameter_value_member(value: &ParameterValue) -> &'static str {
    match value {
        ParameterValue::Integer(_) => "Integer",
        ParameterValue::IntegerList(_) => "IntegerList",
        ParameterValue::Double(_) => "Double",
        ParameterValue::String(_) => "String",
        ParameterValue::StringList(_) => "StringList",
        ParameterValue::Boolean(_) => "Boolean",
        ParameterValue::Enum(_) => "Enum",
        ParameterValue::EnumList(_) => "EnumList",
    }
}

/// The `ConfigurationOptions` union member a published parameter's options are reported
/// under, as the model names it: the `ParameterValue` member a custom value for that
/// parameter must carry, since "customization options vary based on the data type of
/// the parameter" (`smithy_query(query="doc", shape="ParameterDefinition",
/// member="ConfigurationOptions")`).
pub fn parameter_configuration_member(configuration: &ParameterConfiguration) -> &'static str {
    match configuration {
        ParameterConfiguration::Integer { .. } => "Integer",
        ParameterConfiguration::StringList { .. } => "StringList",
        ParameterConfiguration::Boolean { .. } => "Boolean",
    }
}

/// The message an operation addressed by a `SecurityControlId` refuses a parameter name
/// the control publishes no parameter under with: the fixed statement that the control
/// has no such parameter, with the name and the control quoted back as the request
/// supplied them.
pub fn security_control_parameter_does_not_exist_message(
    security_control_id: &str,
    parameter_name: &str,
) -> String {
    format!(
        "A parameter with the provided name: {parameter_name} does not exist for the security control: {security_control_id}."
    )
}

/// Why `value` cannot be the CUSTOM value of `parameter`, or `None` when it satisfies
/// every option the catalog publishes for it: the data type the parameter's
/// `ConfigurationOptions` names, and then the bounds those options carry — the integer
/// range, the list's length and the RE2 expression each of its items must match.
///
/// The single definition of what a custom value must satisfy, so a parameter is checked
/// identically wherever one is set. Neither the model nor the controls reference
/// publishes the text Security Hub CSPM refuses such a value with, so each message
/// states the published option the value violates, with the parameter and the value
/// taken from the request and the bound from the catalogued options.
pub fn custom_parameter_violation(
    parameter: &PublishedControlParameter,
    value: &ParameterValue,
) -> Option<String> {
    let name = parameter.name;
    let expected = parameter_configuration_member(&parameter.configuration);
    let wrong_type = || {
        Some(format!(
            "The value provided for parameter {name} must carry the {expected} member, not {}.",
            parameter_value_member(value)
        ))
    };
    match parameter.configuration {
        ParameterConfiguration::Integer {
            min,
            max,
            default_value: _,
        } => {
            let ParameterValue::Integer(supplied) = value else {
                return wrong_type();
            };
            (*supplied < min || *supplied > max).then(|| {
                format!(
                    "The value provided for parameter {name} must be between {min} and {max}, not {supplied}."
                )
            })
        }
        ParameterConfiguration::StringList {
            max_items,
            re2_expression,
            expression_description,
        } => {
            let ParameterValue::StringList(supplied) = value else {
                return wrong_type();
            };
            let items = i32::try_from(supplied.len()).unwrap_or(i32::MAX);
            if items > max_items {
                return Some(format!(
                    "The value provided for parameter {name} must hold at most {max_items} items, not {items}."
                ));
            }
            // The published RE2 expression is what a user-provided value is validated
            // against ("An RE2 regular expression that Security Hub CSPM uses to
            // validate a user-provided control parameter string"). The expressions are
            // this crate's own catalogued constants, so one that does not compile is a
            // catalog defect rather than a caller's: it is asserted in a debug build
            // and refuses nothing in a release one, so a typo cannot refuse every
            // caller.
            let Ok(expression) = ::regex::Regex::new(re2_expression) else {
                debug_assert!(
                    false,
                    "the catalogued expression of parameter {name} must compile: {re2_expression}"
                );
                return None;
            };
            supplied
                .iter()
                .find(|item| !expression.is_match(item))
                .map(|item| {
                    format!(
                        "The value {item} provided for parameter {name} is not valid: {expression_description}."
                    )
                })
        }
        // "For a boolean parameter, the options are true and false", so a boolean
        // parameter accepts either value and nothing else.
        ParameterConfiguration::Boolean { .. } => match value {
            ParameterValue::Boolean(_) => None,
            _ => wrong_type(),
        },
    }
}

/// Every parameter `entry` publishes, as `SecurityControl$Parameters` reports it: "an
/// object that identifies the name of a control parameter, its current value, and
/// whether it has been customized".
///
/// A parameter this account set carries `CUSTOM` with the value it set; every other
/// parameter carries `DEFAULT` and no value of its own, because for `DEFAULT`
/// "Security Hub CSPM ignores user-provided input for the `Value` field" — the value
/// such a parameter runs with is the default the control's definition publishes. A
/// control that publishes no parameter reports an empty object, the counterpart of the
/// empty `CustomizableProperties` its definition reports.
pub fn security_control_parameters_view(
    entry: &SecurityControlCatalogEntry,
    customization: Option<&SecurityControlCustomization>,
) -> HashMap<String, crate::ops::types::ParameterConfiguration> {
    entry
        .parameters
        .iter()
        .map(|parameter| {
            let custom = customization.and_then(|stored| stored.parameters.get(parameter.name));
            let reported = match custom {
                Some(value) => crate::ops::types::ParameterConfiguration {
                    value_type: PARAMETER_VALUE_TYPE_CUSTOM.to_string(),
                    value: Some(value.clone()),
                },
                None => crate::ops::types::ParameterConfiguration {
                    value_type: PARAMETER_VALUE_TYPE_DEFAULT.to_string(),
                    value: None,
                },
            };
            (parameter.name.to_string(), reported)
        })
        .collect()
}

/// One published control as the modelled `SecurityControl` a read of a control answers
/// with: the control's catalogued metadata — the same values its definition reports, so
/// the two reads cannot describe a control differently — plus what this account made of
/// it: whether the control is enabled, its effective parameters and the reason of the
/// most recent update to them.
pub fn security_control_view(
    region: &str,
    entry: &SecurityControlCatalogEntry,
    customization: Option<&SecurityControlCustomization>,
    security_control_status: &str,
) -> SecurityControl {
    SecurityControl {
        security_control_id: entry.control.control_id.to_string(),
        security_control_arn: security_control_arn(region, entry.control.control_id),
        title: entry.control.title.to_string(),
        description: entry.control.description.to_string(),
        remediation_url: entry.remediation_url.to_string(),
        severity_rating: entry.control.severity_rating.to_string(),
        security_control_status: security_control_status.to_string(),
        update_status: Some(UPDATE_STATUS_READY.to_string()),
        parameters: Some(security_control_parameters_view(entry, customization)),
        last_update_reason: customization.and_then(|stored| stored.last_update_reason.clone()),
        provider: Some(entry.provider.to_string()),
    }
}

/// Whether `entry` is enabled for hub `key`: a security control is enabled while at
/// least one standard the account subscribes to carries the control with the control
/// status `ENABLED`, and disabled once every such subscription has it disabled —
/// including the case where no subscribed standard carries the control at all, which is
/// what an account with no standard enabled reports for every control.
///
/// Read from the subscriptions themselves rather than stored per control, because a
/// control's enablement is a property of the standards an account subscribes to:
/// `UpdateStandardsControl` is what changes it, and disabling a standard takes its
/// controls' statuses with it.
pub fn security_control_status(
    store: &ComAmazonawsSecurityhubSecurityhubapiserviceState,
    key: &HubKey,
    entry: &SecurityControlCatalogEntry,
) -> String {
    let enabled = store
        .standards
        .iter()
        .filter(|(subscription_key, _)| {
            subscription_key.account_id == key.0 && subscription_key.region == key.1
        })
        .filter_map(|(subscription_key, record)| {
            standard_by_arn(&key.1, &subscription_key.standards_arn)
                .map(|definition| (definition, record))
        })
        .filter(|(definition, _)| standard_applies_security_control(definition, entry))
        .any(|(_, record)| {
            // The one definition of the status a subscription holds for a control, so
            // this read and a read of the standards control itself cannot disagree.
            subscription_control_status(record, entry.control.control_id)
                == CONTROL_STATUS_ENABLED
        });
    if enabled {
        CONTROL_STATUS_ENABLED.to_string()
    } else {
        CONTROL_STATUS_DISABLED.to_string()
    }
}
