// SPDX-License-Identifier: CC0-1.0

//! The FIRST-CLASS SERVICE INTEGRATIONS an HTTP api's `AWS_PROXY` integration may name, and
//! the parameters each of them has to be configured with.
//!
//! "Supported only for HTTP API AWS_PROXY integrations. Specifies the AWS service action to
//! invoke" (`smithy_query(query="doc",shape="CreateIntegrationRequest",
//! member="IntegrationSubtype")`): a subtype names ONE action of ONE service, and the
//! integration's `RequestParameters` are "a key-value map specifying parameters that are
//! passed to AWS_PROXY integrations" — the ARGUMENTS of that action rather than an operation
//! on the message (`crate::state::parameter_mapping` holds the other, header-mapping
//! language).
//!
//! An action cannot be called without the arguments it requires, so a subtype whose required
//! arguments the mapping does not define configures a call that could never be made. API
//! Gateway settles that at the WRITE — the create, the update and an import alike — rather
//! than at invoke time, and this module is that reading: which subtypes are first-class
//! ([`FIRST_CLASS_SUBTYPES`]), which parameters each one requires
//! ([`FirstClassSubtype::required_parameters`]), and the one sentence the refusal is worded
//! with ([`unmapped_required_parameters_message`]).
//!
//! The required set of a subtype is the arguments its action cannot be called without — for
//! nearly every one of them exactly the members that action's own input marks REQUIRED, read
//! off the serving service's model, which each entry below cites the query for — spelled as
//! an integration's `RequestParameters` spell a parameter: the action's member name in upper
//! camel case. Nothing in this model enables the PASSTHROUGH the refusal names as the
//! alternative — `PassthroughBehavior` is "Supported only for WebSocket APIs"
//! (`Integration$PassthroughBehavior`) — so an HTTP api's subtype integration is configured
//! the one way, by defining those parameters.
//!
//! A subtype OUTSIDE this catalog is left alone here: the required set of an action this
//! emulator holds no model for is not a fact this service can state, so such a value is
//! stored as the opaque string the member is (`StringWithLengthBetween1And128`, which is all
//! the model constrains it to).

use std::collections::BTreeMap;

/// One first-class service integration: the value `IntegrationSubtype` carries for it, and
/// the parameters an integration of it must define in its mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstClassSubtype {
    /// The subtype as a request spells it, `<Service>-<Action>`.
    pub name: &'static str,
    /// The parameters the action requires, in the order the refusal lists them: the members
    /// its own input marks required, upper camel case as a mapping spells them.
    pub required_parameters: &'static [&'static str],
}

/// `EventBridge-PutEvents`. The parameters are the members of one `PutEventsRequestEntry`, of
/// which an event names its origin (`Source`), its kind (`DetailType`) and its payload
/// (`Detail`) — the three EventBridge itself refuses an entry without.
pub const EVENT_BRIDGE_PUT_EVENTS: FirstClassSubtype = FirstClassSubtype {
    name: "EventBridge-PutEvents",
    required_parameters: &["Detail", "DetailType", "Source"],
};

/// `AppConfig-GetConfiguration`
/// (`smithy_query(query="operation",operation="GetConfiguration")` on the AppConfig model:
/// `Application`, `Environment`, `Configuration` and `ClientId` are its required members).
pub const APP_CONFIG_GET_CONFIGURATION: FirstClassSubtype = FirstClassSubtype {
    name: "AppConfig-GetConfiguration",
    required_parameters: &["Application", "ClientId", "Configuration", "Environment"],
};

/// `Kinesis-PutRecord`: the stream the record is put on, the record itself and the key it is
/// partitioned by (`smithy_query(query="operation",operation="PutRecord")` on the Kinesis
/// model, whose `Data` is required; `StreamName` and `PartitionKey` are this subtype's only
/// way to name the stream and the shard, so a mapping defining neither names no put).
pub const KINESIS_PUT_RECORD: FirstClassSubtype = FirstClassSubtype {
    name: "Kinesis-PutRecord",
    required_parameters: &["Data", "PartitionKey", "StreamName"],
};

/// `SQS-SendMessage` (`smithy_query(query="operation",operation="SendMessage")` on the Amazon
/// SQS model: `QueueUrl` and `MessageBody` are its required members).
pub const SQS_SEND_MESSAGE: FirstClassSubtype = FirstClassSubtype {
    name: "SQS-SendMessage",
    required_parameters: &["MessageBody", "QueueUrl"],
};

/// `SQS-ReceiveMessage` (`ReceiveMessage` requires `QueueUrl`).
pub const SQS_RECEIVE_MESSAGE: FirstClassSubtype = FirstClassSubtype {
    name: "SQS-ReceiveMessage",
    required_parameters: &["QueueUrl"],
};

/// `SQS-DeleteMessage` (`DeleteMessage` requires `QueueUrl` and `ReceiptHandle`).
pub const SQS_DELETE_MESSAGE: FirstClassSubtype = FirstClassSubtype {
    name: "SQS-DeleteMessage",
    required_parameters: &["QueueUrl", "ReceiptHandle"],
};

/// `SQS-PurgeQueue` (`PurgeQueue` requires `QueueUrl`).
pub const SQS_PURGE_QUEUE: FirstClassSubtype = FirstClassSubtype {
    name: "SQS-PurgeQueue",
    required_parameters: &["QueueUrl"],
};

/// `StepFunctions-StartExecution`
/// (`smithy_query(query="operation",operation="StartExecution")` on the Step Functions model:
/// `stateMachineArn` is its one required member).
pub const STEP_FUNCTIONS_START_EXECUTION: FirstClassSubtype = FirstClassSubtype {
    name: "StepFunctions-StartExecution",
    required_parameters: &["StateMachineArn"],
};

/// `StepFunctions-StartSyncExecution`, whose input declares the same required member.
pub const STEP_FUNCTIONS_START_SYNC_EXECUTION: FirstClassSubtype = FirstClassSubtype {
    name: "StepFunctions-StartSyncExecution",
    required_parameters: STEP_FUNCTIONS_START_EXECUTION.required_parameters,
};

/// `StepFunctions-StopExecution`
/// (`smithy_query(query="operation",operation="StopExecution")`: `executionArn` is its one
/// required member — the run to stop, which nothing else in that input names).
pub const STEP_FUNCTIONS_STOP_EXECUTION: FirstClassSubtype = FirstClassSubtype {
    name: "StepFunctions-StopExecution",
    required_parameters: &["ExecutionArn"],
};

/// Every first-class subtype this service states the required parameters of.
pub const FIRST_CLASS_SUBTYPES: &[FirstClassSubtype] = &[
    APP_CONFIG_GET_CONFIGURATION,
    EVENT_BRIDGE_PUT_EVENTS,
    KINESIS_PUT_RECORD,
    SQS_DELETE_MESSAGE,
    SQS_PURGE_QUEUE,
    SQS_RECEIVE_MESSAGE,
    SQS_SEND_MESSAGE,
    STEP_FUNCTIONS_START_EXECUTION,
    STEP_FUNCTIONS_START_SYNC_EXECUTION,
    STEP_FUNCTIONS_STOP_EXECUTION,
];

/// The first-class subtype `subtype` names, or `None` for a value naming none of them.
///
/// Matched exactly, because an `IntegrationSubtype` is the action's own name: a value spelled
/// any other way names no action this service states a required set for.
pub fn first_class_subtype(subtype: &str) -> Option<&'static FirstClassSubtype> {
    FIRST_CLASS_SUBTYPES
        .iter()
        .find(|known| known.name == subtype)
}

/// How API Gateway words the refusal of a subtype integration whose mapping does not define
/// the parameters the action requires.
///
/// Every part of the sentence is fixed but the subtype, which is the one the request named,
/// and the bracketed list, which is that subtype's own required set — all of it, because the
/// sentence asks for all of the parameters to be defined rather than naming the ones left
/// out.
pub fn unmapped_required_parameters_message(subtype: &str, required: &[&str]) -> String {
    format!(
        "Operation: {subtype} requires enabling passthrough, or defining all of the \
         following parameters in the parameter mapping: [{}]",
        required.join(", ")
    )
}

/// `Ok(())`, or why this integration's `IntegrationSubtype` and `RequestParameters` do not
/// configure a call the action could be made with — the message for the writing operation's
/// `BadRequestException`.
///
/// A parameter is DEFINED when the mapping holds a non-blank value under its name: a key
/// present with nothing behind it supplies the action no argument, so it is no definition of
/// one. Parameter names are compared exactly, as the action's own members are spelled.
pub fn validate_subtype_request_parameters(
    integration_subtype: Option<&str>,
    request_parameters: &BTreeMap<String, String>,
) -> Result<(), String> {
    let Some(subtype) = integration_subtype
        .map(str::trim)
        .filter(|subtype| !subtype.is_empty())
    else {
        return Ok(());
    };
    let Some(known) = first_class_subtype(subtype) else {
        return Ok(());
    };
    let defined = |parameter: &&str| {
        request_parameters
            .get(*parameter)
            .is_some_and(|value| !value.trim().is_empty())
    };
    if known.required_parameters.iter().all(defined) {
        return Ok(());
    }
    Err(unmapped_required_parameters_message(
        subtype,
        known.required_parameters,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    /// RULE: every first-class subtype is configured only when its mapping defines each
    /// parameter its action requires, and refused otherwise.
    #[test]
    fn a_subtype_is_configured_only_with_the_parameters_its_action_requires() {
        for subtype in FIRST_CLASS_SUBTYPES {
            // The mapping that defines every required parameter is accepted...
            let complete: Vec<(&str, &str)> = subtype
                .required_parameters
                .iter()
                .map(|parameter| (*parameter, "$request.body.value"))
                .collect();
            let validated =
                validate_subtype_request_parameters(Some(subtype.name), &mapping(&complete));
            assert_eq!(
                validated,
                Ok(()),
                "case {:?}: a mapping defining {:?} must be accepted, got {validated:?}",
                subtype.name,
                subtype.required_parameters
            );

            // ... and dropping any ONE of them, or all of them, is refused with the sentence
            // that names the subtype and its whole required set.
            let expected = Err(unmapped_required_parameters_message(
                subtype.name,
                subtype.required_parameters,
            ));
            for dropped in subtype.required_parameters {
                let partial: Vec<(&str, &str)> = complete
                    .iter()
                    .copied()
                    .filter(|(parameter, _)| parameter != dropped)
                    .collect();
                let validated =
                    validate_subtype_request_parameters(Some(subtype.name), &mapping(&partial));
                assert_eq!(
                    validated, expected,
                    "case {:?} without {dropped:?}: expected {expected:?}, got {validated:?}",
                    subtype.name
                );
            }
            let validated = validate_subtype_request_parameters(Some(subtype.name), &mapping(&[]));
            assert_eq!(
                validated, expected,
                "case {:?} with no mapping at all: expected {expected:?}, got {validated:?}",
                subtype.name
            );
        }
    }

    /// RULE: a parameter is defined by a VALUE, and only under the name the action spells —
    /// so a blank value, another casing and an optional parameter's name are all no
    /// definition of a required one.
    #[test]
    fn only_a_value_under_the_parameter_s_own_name_defines_it() {
        /// One row: the case, the mapping it holds, and whether it configures the subtype.
        struct Row {
            case: &'static str,
            entries: Vec<(&'static str, &'static str)>,
            accepted: bool,
        }
        let subtype = STEP_FUNCTIONS_STOP_EXECUTION.name;
        let expected = Err(unmapped_required_parameters_message(
            subtype,
            STEP_FUNCTIONS_STOP_EXECUTION.required_parameters,
        ));
        let rows = [
            Row {
                case: "the parameter under its own name",
                entries: vec![("ExecutionArn", "$request.body.arn")],
                accepted: true,
            },
            Row {
                case: "a static value",
                entries: vec![("ExecutionArn", "a-static-value")],
                accepted: true,
            },
            Row {
                case: "an empty value",
                entries: vec![("ExecutionArn", "")],
                accepted: false,
            },
            Row {
                case: "a blank value",
                entries: vec![("ExecutionArn", "   ")],
                accepted: false,
            },
            Row {
                case: "another casing of the name",
                entries: vec![("executionarn", "$request.body.arn")],
                accepted: false,
            },
            Row {
                case: "the optional parameters only",
                entries: vec![("Cause", "stopped"), ("Error", "by-the-route")],
                accepted: false,
            },
            Row {
                case: "the required parameter beside an optional one",
                entries: vec![("Cause", "stopped"), ("ExecutionArn", "$request.body.arn")],
                accepted: true,
            },
        ];
        for row in &rows {
            let case = row.case;
            let validated =
                validate_subtype_request_parameters(Some(subtype), &mapping(&row.entries));
            let wanted = if row.accepted {
                Ok(())
            } else {
                expected.clone()
            };
            assert_eq!(
                validated, wanted,
                "case {case:?}: expected {wanted:?}, got {validated:?}"
            );
        }
    }

    /// RULE: only a first-class subtype has a required set, so an integration naming none, or
    /// naming a value this service states no action for, is left alone.
    #[test]
    fn an_integration_naming_no_first_class_subtype_is_left_alone() {
        // Rows differ along the VALUE of the member: absent, empty, blank, a subtype spelled
        // in another casing, one of a service with no catalogued action, and a value that is
        // no `<Service>-<Action>` pair at all.
        let rows: &[(&str, Option<&str>)] = &[
            ("no subtype at all", None),
            ("an empty subtype", Some("")),
            ("a blank subtype", Some("   ")),
            ("another casing of a known subtype", Some("sqs-sendmessage")),
            (
                "an action of a service with no catalogued subtype",
                Some("S3-PutObject"),
            ),
            ("a value naming no action", Some("not-an-action")),
        ];
        for (case, subtype) in rows {
            let validated = validate_subtype_request_parameters(*subtype, &mapping(&[]));
            assert_eq!(
                validated,
                Ok(()),
                "case {case:?}: expected the mapping to be left alone, got {validated:?}"
            );
        }
    }
}
