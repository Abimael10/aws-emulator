// SPDX-License-Identifier: CC0-1.0

//! What a subscriber's `InvokeConfiguration` has to name before a subscriber can
//! be minted from it: the parameters member that goes with the target, and the
//! account the two ARNs it carries belong to.
//!
//! `InvokeConfiguration` is "Configuration for how the subscriber invokes its
//! target": a required `TargetArn`, a required `RoleArn` and the ONE
//! service-specific parameter block the target takes. Each block names the API
//! the delivery is made through — `SqsParameters` is "forwarded to the SQS
//! SendMessageBatch API", `LambdaParameters` "to the Lambda Invoke API",
//! `SnsParameters` "to the SNS PublishBatch API", `KinesisParameters` "to the
//! Kinesis PutRecords API", `StepFunctionsParameters` "to the Step Functions
//! StartExecution or StartSyncExecution API", `HttpParameters` to an "(API
//! Gateway / API Destination)" endpoint, `EventBusV2Parameters` to "another
//! EventBridge event bus" and `UniversalTargetParameters` to a target "in the
//! format arn:aws:events:::aws-sdk:{service}:{apiAction}" — so the block a
//! target takes follows from the target's own ARN, and a request naming any
//! other block names a call that cannot be made to it. That is a request
//! parameter that is not valid, so it is refused rather than stored.
//!
//! Both ARNs are read against the CALLING account: `InvokeConfiguration$RoleArn`
//! is "IAM role the service assumes to invoke the target. Must belong to the
//! calling account", and a subscriber is created in the account of its own bus,
//! so a target in another account is not one it may be pointed at.
//!
//! Nothing here writes, and nothing here is a refusal's envelope: each reading
//! hands back the whole `Message` of the refusal (or the fact of it), and the
//! handler that asked chooses the modelled exception it travels in.

use crate::ops::types::{BatchConfiguration, InvokeConfiguration, Transformer};

/// The member of `InvokeConfiguration` an AWS Lambda function target takes, as
/// the model spells it — the name a refusal names the request's own member by.
pub const LAMBDA_PARAMETERS_MEMBER: &str = "LambdaParameters";

/// The member an Amazon SQS queue target takes.
pub const SQS_PARAMETERS_MEMBER: &str = "SqsParameters";

/// The member an Amazon SNS topic target takes.
pub const SNS_PARAMETERS_MEMBER: &str = "SnsParameters";

/// The member an Amazon Kinesis Data Streams target takes.
pub const KINESIS_PARAMETERS_MEMBER: &str = "KinesisParameters";

/// The member an AWS Step Functions state-machine target takes.
pub const STEP_FUNCTIONS_PARAMETERS_MEMBER: &str = "StepFunctionsParameters";

/// The member an HTTP target — an API Gateway stage or an EventBridge API
/// destination — takes.
pub const HTTP_PARAMETERS_MEMBER: &str = "HttpParameters";

/// The member a universal (aws-sdk) target takes.
pub const UNIVERSAL_TARGET_PARAMETERS_MEMBER: &str = "UniversalTargetParameters";

/// The member another event bus of this service takes.
pub const EVENT_BUS_V2_PARAMETERS_MEMBER: &str = "EventBusV2Parameters";

/// What a refusal names where the target takes NO parameters member at all,
/// in place of the member another target would take.
///
/// A target this service documents no parameters member for is invoked without
/// parameters of the subscriber's choosing — an event bus of the PREVIOUS
/// generation (`…:event-bus/<name>`, which is not this service's
/// `event-busv2` resource) is one such — so every member is unsupported for it
/// rather than one being expected.
pub const NO_TARGET_PARAMETERS: &str = "no target parameters";

/// The `arnNamespace` of AWS Lambda, the vendor field of a function's ARN.
const LAMBDA_ARN_NAMESPACE: &str = "lambda";

/// The `arnNamespace` of Amazon Kinesis Data Streams.
const KINESIS_ARN_NAMESPACE: &str = "kinesis";

/// The `arnNamespace` of AWS Step Functions, whose state machines and executions
/// are written under `states`.
const STEP_FUNCTIONS_ARN_NAMESPACE: &str = "states";

/// The `arnNamespace` an Amazon API Gateway stage is addressed under, the HTTP
/// endpoint form of a target.
const EXECUTE_API_ARN_NAMESPACE: &str = "execute-api";

/// The resource type an EventBridge API destination is written under, the other
/// HTTP endpoint form: a target of this service's own namespace that is neither
/// a bus nor a universal call.
const API_DESTINATION_RESOURCE_TYPE: &str = "api-destination";

/// The opening of the resource field of a UNIVERSAL target's ARN:
/// `UniversalTargetParameters` is "Used when TargetArn is in the format
/// arn:aws:events:::aws-sdk:{service}:{apiAction}", so the resource names the
/// SDK call rather than a resource of this service.
const UNIVERSAL_TARGET_RESOURCE_PREFIX: &str = "aws-sdk:";

/// The resource type of an event bus of the PREVIOUS generation of EventBridge:
/// a bus of the `events` namespace written as `event-bus/<name>`, which is not
/// this service's own `event-busv2` resource
/// ([`super::ARN_RESOURCE_TYPE`]).
const CLASSIC_EVENT_BUS_RESOURCE_TYPE: &str = "event-bus";

/// The whole `Message` of the refusal a subscriber delivering to a CLASSIC
/// EventBridge target under any transformation but the raw one is answered
/// with.
///
/// A classic bus takes the event as the previous generation of EventBridge
/// represents it, so the only transformation such a target supports is the one
/// that hands it the event untouched (`TransformerType::RAW`, "Deliver the raw
/// event payload"). Fixed text throughout: it names the one type that IS
/// supported rather than the one the request asked for.
pub const CLASSIC_TARGET_TRANSFORMER_MESSAGE: &str =
    "Classic EventBridge targets only support Transformer.Type RAW";

/// The whole `Message` of the `AccessDeniedException` a subscriber request
/// naming a role the calling account does not hold is refused with.
///
/// `AccessDeniedException` is "The caller does not have the permissions required
/// to perform the operation", and this is how that refusal is worded: fixed text
/// throughout, naming neither the role nor the target, because a caller that may
/// not act under a value it supplied is told no more than that.
pub const ACCESS_DENIED_MESSAGE: &str = "Access denied.";

/// Whether `role_arn` names no IAM role of the CALLING account, which is a role
/// this service cannot assume to invoke anything:
/// `InvokeConfiguration$RoleArn` is "IAM role the service assumes to invoke the
/// target. Must belong to the calling account".
///
/// The reading is the tree's one reading of that sentence
/// ([`::service_common::iam_role::role_name`]), the same one the delivery path
/// takes before it acts under a subscriber's role — so a role no delivery could
/// ever be made under is refused at the call that would have configured it,
/// rather than at every delivery it would never make.
#[must_use]
pub fn role_is_not_the_callers(role_arn: &str) -> bool {
    ::service_common::iam_role::role_name(role_arn).is_none()
}

/// Whether `arn` names an event bus of the PREVIOUS generation of EventBridge.
#[must_use]
pub fn is_classic_event_bus_target(arn: &str) -> bool {
    ::service_common::arn::parse(arn).is_some_and(|parsed| {
        parsed.vendor == super::ARN_SERVICE
            && parsed.resource_type() == CLASSIC_EVENT_BUS_RESOURCE_TYPE
    })
}

/// Why the transformation `transformer` cannot be applied to the target
/// `invoke_configuration` names, or `None` when it can.
///
/// A transformer a request does not name is the service's own
/// ([`super::TRANSFORMER_TYPE_RAW`]), which every target supports, so only a
/// transformation the caller asked for can be refused here.
#[must_use]
pub fn classic_target_transformer_fault(
    invoke_configuration: &InvokeConfiguration,
    transformer: Option<&Transformer>,
) -> Option<String> {
    if !is_classic_event_bus_target(&invoke_configuration.target_arn) {
        return None;
    }
    let requested = transformer.and_then(|transformer| transformer.r#type.as_deref())?;
    (requested != super::TRANSFORMER_TYPE_RAW)
        .then(|| CLASSIC_TARGET_TRANSFORMER_MESSAGE.to_string())
}

/// The parameters member the target `arn` takes, or `None` when this service
/// documents no member for the service that ARN names.
///
/// Read off the ARN's VENDOR field — the service namespace, which is what says
/// whose resource is named — through the crate's one ARN parse, so the two forms
/// of this service's own namespace (another event bus, and the `aws-sdk:` call of
/// a universal target) are told apart by the resource they name.
#[must_use]
pub fn target_parameters_member(arn: &str) -> Option<&'static str> {
    let parsed = ::service_common::arn::parse(arn)?;
    match parsed.vendor {
        LAMBDA_ARN_NAMESPACE => Some(LAMBDA_PARAMETERS_MEMBER),
        super::subscriber_delivery::SQS_ARN_NAMESPACE => Some(SQS_PARAMETERS_MEMBER),
        super::subscriber_delivery::SNS_ARN_NAMESPACE => Some(SNS_PARAMETERS_MEMBER),
        KINESIS_ARN_NAMESPACE => Some(KINESIS_PARAMETERS_MEMBER),
        STEP_FUNCTIONS_ARN_NAMESPACE => Some(STEP_FUNCTIONS_PARAMETERS_MEMBER),
        EXECUTE_API_ARN_NAMESPACE => Some(HTTP_PARAMETERS_MEMBER),
        super::ARN_SERVICE if parsed.resource.starts_with(UNIVERSAL_TARGET_RESOURCE_PREFIX) => {
            Some(UNIVERSAL_TARGET_PARAMETERS_MEMBER)
        }
        super::ARN_SERVICE if parsed.resource_type() == super::ARN_RESOURCE_TYPE => {
            Some(EVENT_BUS_V2_PARAMETERS_MEMBER)
        }
        super::ARN_SERVICE if parsed.resource_type() == API_DESTINATION_RESOURCE_TYPE => {
            Some(HTTP_PARAMETERS_MEMBER)
        }
        _ => None,
    }
}

/// The parameters members `invoke_configuration` carries, in the order the model
/// declares them — so a request carrying more than one is read the same way
/// twice.
fn supplied_parameters_members(
    invoke_configuration: &InvokeConfiguration,
) -> impl Iterator<Item = &'static str> + '_ {
    [
        (
            LAMBDA_PARAMETERS_MEMBER,
            invoke_configuration.lambda_parameters.is_some(),
        ),
        (
            SQS_PARAMETERS_MEMBER,
            invoke_configuration.sqs_parameters.is_some(),
        ),
        (
            SNS_PARAMETERS_MEMBER,
            invoke_configuration.sns_parameters.is_some(),
        ),
        (
            KINESIS_PARAMETERS_MEMBER,
            invoke_configuration.kinesis_parameters.is_some(),
        ),
        (
            STEP_FUNCTIONS_PARAMETERS_MEMBER,
            invoke_configuration.step_functions_parameters.is_some(),
        ),
        (
            HTTP_PARAMETERS_MEMBER,
            invoke_configuration.http_parameters.is_some(),
        ),
        (
            UNIVERSAL_TARGET_PARAMETERS_MEMBER,
            invoke_configuration.universal_target_parameters.is_some(),
        ),
        (
            EVENT_BUS_V2_PARAMETERS_MEMBER,
            invoke_configuration.event_bus_v2_parameters.is_some(),
        ),
    ]
    .into_iter()
    .filter_map(|(member, supplied)| supplied.then_some(member))
}

/// Why `invoke_configuration` names a parameters member its own target cannot be
/// invoked through, or `None` when every member it carries is one that target
/// takes.
///
/// A configuration that carries NO parameters member names no call of its own
/// and is left to the service's own defaults, which is why it is not a fault: the
/// members are optional, and the required part of the configuration is the target
/// and the role. A member for ANOTHER service is a fault whatever else the
/// request carries, and so is ANY member on a target this service documents no
/// parameters member for — a target of a service whose call carries no
/// parameters of a subscriber's choosing takes none
/// ([`NO_TARGET_PARAMETERS`]).
///
/// The `Some` is the whole `Message` of the refusal: the member the request
/// named, the target it named it for, and what that target takes instead.
#[must_use]
pub fn target_parameters_fault(invoke_configuration: &InvokeConfiguration) -> Option<String> {
    ::service_common::arn::parse(&invoke_configuration.target_arn)?;
    let expected = target_parameters_member(&invoke_configuration.target_arn);
    let supplied = supplied_parameters_members(invoke_configuration)
        .find(|member| Some(*member) != expected)?;
    Some(format!(
        "{supplied} is not supported for target {}. Expected {}",
        invoke_configuration.target_arn,
        expected.unwrap_or(NO_TARGET_PARAMETERS)
    ))
}

/// The scalar members of `invoke_configuration`'s parameters blocks that carry a
/// LITERAL OR AN EXPRESSION, each with the path a refusal names it by.
///
/// Those members are the ones documented as accepting "a literal or a JSONata
/// expression": every string-typed scalar of a target's parameters block. The
/// collections beside them (a message-attribute map, a header or query map, a
/// path-parameter list) carry their own values and are not read here, and
/// `UniversalTargetParameters$Input` is not one either — it is a whole JSON
/// document which "Supports {% … %} JSONata expressions for dynamic values", so
/// the delimiters may appear INSIDE it.
fn configured_values(invoke_configuration: &InvokeConfiguration) -> Vec<(String, &str)> {
    let mut values: Vec<(String, &str)> = Vec::new();
    // Each row is a member of the model, named as the model names it.
    if let Some(parameters) = invoke_configuration.lambda_parameters.as_ref() {
        for (member, value) in [
            ("Qualifier", parameters.qualifier.as_deref()),
            (
                "DurableExecutionName",
                parameters.durable_execution_name.as_deref(),
            ),
            ("TenantId", parameters.tenant_id.as_deref()),
            (
                "InvocationTimeoutSeconds",
                parameters.invocation_timeout_seconds.as_deref(),
            ),
        ] {
            if let Some(value) = value {
                values.push((format!("{LAMBDA_PARAMETERS_MEMBER}.{member}"), value));
            }
        }
    }
    if let Some(parameters) = invoke_configuration.sqs_parameters.as_ref() {
        for (member, value) in [
            ("MessageGroupId", parameters.message_group_id.as_deref()),
            (
                "MessageDeduplicationId",
                parameters.message_deduplication_id.as_deref(),
            ),
            ("DelaySeconds", parameters.delay_seconds.as_deref()),
        ] {
            if let Some(value) = value {
                values.push((format!("{SQS_PARAMETERS_MEMBER}.{member}"), value));
            }
        }
    }
    if let Some(parameters) = invoke_configuration.sns_parameters.as_ref() {
        for (member, value) in [
            ("MessageGroupId", parameters.message_group_id.as_deref()),
            (
                "MessageDeduplicationId",
                parameters.message_deduplication_id.as_deref(),
            ),
            ("Subject", parameters.subject.as_deref()),
            ("MessageStructure", parameters.message_structure.as_deref()),
        ] {
            if let Some(value) = value {
                values.push((format!("{SNS_PARAMETERS_MEMBER}.{member}"), value));
            }
        }
    }
    if let Some(parameters) = invoke_configuration.kinesis_parameters.as_ref() {
        for (member, value) in [
            ("PartitionKey", parameters.partition_key.as_deref()),
            ("ExplicitHashKey", parameters.explicit_hash_key.as_deref()),
        ] {
            if let Some(value) = value {
                values.push((format!("{KINESIS_PARAMETERS_MEMBER}.{member}"), value));
            }
        }
    }
    if let Some(parameters) = invoke_configuration.step_functions_parameters.as_ref() {
        for (member, value) in [
            ("Name", parameters.name.as_deref()),
            ("TraceHeader", parameters.trace_header.as_deref()),
            (
                "InvocationTimeoutSeconds",
                parameters.invocation_timeout_seconds.as_deref(),
            ),
        ] {
            if let Some(value) = value {
                values.push((
                    format!("{STEP_FUNCTIONS_PARAMETERS_MEMBER}.{member}"),
                    value,
                ));
            }
        }
    }
    if let Some(parameters) = invoke_configuration.http_parameters.as_ref() {
        if let Some(value) = parameters.invocation_timeout_seconds.as_deref() {
            values.push((
                format!("{HTTP_PARAMETERS_MEMBER}.InvocationTimeoutSeconds"),
                value,
            ));
        }
    }
    if let Some(parameters) = invoke_configuration.universal_target_parameters.as_ref() {
        if let Some(value) = parameters.invocation_timeout_seconds.as_deref() {
            values.push((
                format!("{UNIVERSAL_TARGET_PARAMETERS_MEMBER}.InvocationTimeoutSeconds"),
                value,
            ));
        }
    }
    values
}

/// Why a configured value of `invoke_configuration` is neither a literal nor an
/// expression this service can evaluate, or `None` when every one of them is
/// one.
///
/// A member of a target's parameters block carries "a literal or a JSONata
/// expression", and the language delimits one expression with `{%` and `%}`, so
/// a value that reaches for those delimiters is held to being ONE whole
/// expression: the delimiters around the whole value with nothing outside them,
/// and a text the language admits between them. A value that does not reach for
/// them at all is the literal it is written as, and nothing is asked of it.
///
/// The `Some` is the whole `Message` of the refusal, whose one variable part is
/// the path of the member at fault.
#[must_use]
pub fn configured_expression_fault(invoke_configuration: &InvokeConfiguration) -> Option<String> {
    configured_values(invoke_configuration)
        .into_iter()
        .find_map(|(path, value)| configured_value_fault(&path, value))
}

/// Why the value of the member at `path` is not one of the two forms, or `None`
/// when it is.
fn configured_value_fault(path: &str, value: &str) -> Option<String> {
    let open = super::EXPRESSION_OPEN;
    let close = super::EXPRESSION_CLOSE;
    if !value.contains(open) && !value.contains(close) {
        return None;
    }
    let whole = value.len() > open.len() + close.len()
        && value == value.trim()
        && value.starts_with(open)
        && value.ends_with(close);
    if whole && super::expression_parse_failure(value).is_none() {
        return None;
    }
    Some(format!(
        "{path} is not a valid JSONata expression. It must be wrapped in {open} {close} \
         delimiters with no surrounding whitespace and parse as JSONata."
    ))
}

/// The whole `Message` of the refusal a subscriber naming a universal target
/// this service does not invoke is answered with, after the API action it named.
///
/// A universal target delivers an event by MAKING the SDK call its ARN names
/// (`arn:aws:events:::aws-sdk:{service}:{apiAction}`), so the call has to be one
/// that does something with the event: an action that only READS changes
/// nothing, and is not an API a subscriber may be pointed at. The variable part
/// is the action the request's own target ARN named.
const UNSUPPORTED_API_SUFFIX: &str = " API is not supported";

/// The SDK service names EVENTBRIDGE ITSELF is called under, as a universal
/// target's ARN spells the service of its call.
///
/// A subscriber that delivers onto an event bus is the natively modelled target
/// of this service — `EventBusV2Parameters`, "Parameters for forwarding events
/// to another EventBridge event bus" — so the SDK form of that same delivery is
/// not a universal call this service makes: a request naming one is refused, and
/// the refusal names the whole call rather than its action alone, because it is
/// the call that is unsupported and not the action wherever it appears. Which
/// SDK names reach THIS service is this service's own fact, which is why it is
/// stated here and not with the called services' APIs.
const EVENTBRIDGE_SDK_SERVICES: [&str; 2] = ["eventbridge", "events"];

/// The API action a UNIVERSAL target's ARN names, or `None` when `arn` is not
/// one: `arn:aws:events:::aws-sdk:{service}:{apiAction}`, the form
/// `UniversalTargetParameters` documents.
#[must_use]
pub fn universal_target_api_action(arn: &str) -> Option<&str> {
    universal_target_call(arn).map(|(_service, action)| action)
}

/// The SDK service and API action a UNIVERSAL target's ARN names, each as that
/// ARN spells it.
///
/// The ARN's own grammar is THIS service's — it is written under this service's
/// namespace, as EventBridge Scheduler's universal target is written under
/// `scheduler` — so the split is here, while what the pair it names IS comes
/// from the tree's one statement of the called services' APIs
/// ([`::service_common::universal_target_apis`]).
fn universal_target_call(arn: &str) -> Option<(&str, &str)> {
    let parsed = ::service_common::arn::parse(arn)?;
    if parsed.vendor != super::ARN_SERVICE {
        return None;
    }
    let call = parsed.resource.strip_prefix(UNIVERSAL_TARGET_RESOURCE_PREFIX)?;
    let (service, action) = call.split_once(':')?;
    (!service.is_empty() && !action.is_empty()).then_some((service, action))
}

/// Why the universal target `invoke_configuration` names is not one this service
/// invokes, or `None` when it is — including for every target that is not a
/// universal one, which this says nothing about.
///
/// Two calls are not universal targets: one whose API only READS, and one
/// addressed at EventBridge itself ([`EVENTBRIDGE_SDK_SERVICES`]), which is the
/// natively modelled event-bus target instead. Beside those, the action has to
/// be an API the named service HAS. Which operations a service has and which of
/// them only read are the CALLED service's facts, read from the tree's one
/// statement of them ([`::service_common::universal_target_apis`]); a service
/// that statement holds nothing for is not judged, because AWS's universal
/// targets span far more services than this emulator hosts.
///
/// The `Some` is the whole `Message` of the refusal, whose variable parts are
/// what the target ARN named — the action for the first, the whole call for the
/// second, the action and the service for the third.
#[must_use]
pub fn universal_target_api_fault(invoke_configuration: &InvokeConfiguration) -> Option<String> {
    let (service, action) = universal_target_call(&invoke_configuration.target_arn)?;
    if EVENTBRIDGE_SDK_SERVICES.contains(&service.to_ascii_lowercase().as_str()) {
        return Some(format!(
            "{UNIVERSAL_TARGET_RESOURCE_PREFIX}{service}:{action}{UNSUPPORTED_API_SUFFIX}"
        ));
    }
    let apis = super::apis_of(service)?;
    if apis.only_reads(action) {
        return Some(format!("{action}{UNSUPPORTED_API_SUFFIX}"));
    }
    (!apis.names(action))
        .then(|| format!("The api {action} is not valid for the service {service}"))
}

/// The whole `Message` of the refusal a universal target whose `Input` does not
/// carry the members its API requires is answered with, before the members it
/// is missing.
///
/// `UniversalTargetParameters$Input` is the "JSON string or JSONata expression
/// that produces the API request", so an Input stated as a JSON document IS that
/// request: a request missing a member the API requires could never be made, and
/// a target that states no `UniversalTargetParameters` at all states no request
/// either, so every required member is missing from it. The variable part is the
/// members that are missing, in the order the API's own model declares them
/// required (alphabetically), so one missing set reads one way.
const MISSING_INPUT_FIELDS_PREFIX: &str =
    "UniversalTargetParameters.Input is missing required field(s): ";

/// Why the `Input` of the universal target `invoke_configuration` names cannot
/// be the request its API takes, or `None` when it can.
///
/// Read where both are known: the call is a universal one, and the tree's
/// statement of the called service's APIs holds the required members of this one
/// ([`::service_common::universal_target_apis::ServiceApis::required_input_of`]).
/// An `Input` that is an EXPRESSION is not read — "Supports {% … %} JSONata
/// expressions for dynamic values", and what one produces is not known until an
/// event is evaluated over it — while a target carrying no parameters at all
/// states no request, and every required member is missing from that.
#[must_use]
pub fn universal_target_input_fault(invoke_configuration: &InvokeConfiguration) -> Option<String> {
    let (service, action) = universal_target_call(&invoke_configuration.target_arn)?;
    let required = super::apis_of(service)?.required_input_of(action)?;
    let members = match invoke_configuration.universal_target_parameters.as_ref() {
        // No parameters at all: no request is stated, so nothing of it is there.
        None => ::serde_json::Map::new(),
        Some(parameters) => {
            let document = ::serde_json::from_str::<::serde_json::Value>(&parameters.input).ok()?;
            document.as_object()?.clone()
        }
    };
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|member| !members.contains_key(*member))
        .collect();
    (!missing.is_empty()).then(|| format!("{MISSING_INPUT_FIELDS_PREFIX}{}", missing.join(", ")))
}

/// The whole `Message` of the refusal a subscriber on a UNIVERSAL target that
/// does not state its batching is answered with.
///
/// A universal target is handed the request its `Input` produces, and that
/// request is made for a BATCH of events, so how many events one call carries
/// and how long one waits to fill are part of configuring such a target rather
/// than values the service may choose: both members of `BatchConfiguration`
/// have to be stated. Fixed text throughout, naming the members it is about.
pub const UNIVERSAL_TARGET_BATCH_MESSAGE: &str = "BatchConfiguration must be provided with valid \
     MaxBatchSize and MaxBatchWindow values for USI targets";

/// Why the batching `batch_configuration` states is not one a universal target
/// can be invoked under, or `None` when it is — including for every target that
/// is not a universal one, whose batching the service settles itself.
#[must_use]
pub fn universal_target_batch_fault(
    invoke_configuration: &InvokeConfiguration,
    batch_configuration: Option<&BatchConfiguration>,
) -> Option<String> {
    universal_target_call(&invoke_configuration.target_arn)?;
    let stated = batch_configuration.is_some_and(|batching| {
        batching.max_batch_size.is_some() && batching.max_batch_window_in_seconds.is_some()
    });
    (!stated).then(|| UNIVERSAL_TARGET_BATCH_MESSAGE.to_string())
}

/// The whole `Message` of the refusal a target invocation timeout outside its
/// documented range is answered with.
///
/// `InvocationTimeoutSeconds` is "Timeout in seconds for each invocation of the
/// target (1-30, default 30)" on the parameter blocks that document a range for
/// it, and the member is string-typed only so the value MAY be an expression —
/// so a literal outside that range is a request parameter that is not valid,
/// which no modelled `@range` catches. Fixed text stating the bound, naming no
/// block, because the bound is the same wherever the member is documented with
/// it.
pub const INVOCATION_TIMEOUT_MESSAGE: &str = "InvocationTimeoutSeconds must be between 1 and 30";

/// The member the bound above is about, as the model names it.
const INVOCATION_TIMEOUT_MEMBER: &str = "InvocationTimeoutSeconds";

/// The bottom of that documented range.
const MIN_INVOCATION_TIMEOUT_SECONDS: i64 = 1;

/// The top of that documented range.
const MAX_INVOCATION_TIMEOUT_SECONDS: i64 = 30;

/// The parameter blocks whose `InvocationTimeoutSeconds` documents the 1-30
/// range ([`INVOCATION_TIMEOUT_MESSAGE`]). `LambdaParameters` is not one: its
/// own member documents no range, so nothing is asked of its value here.
const BOUNDED_TIMEOUT_BLOCKS: [&str; 3] = [
    STEP_FUNCTIONS_PARAMETERS_MEMBER,
    HTTP_PARAMETERS_MEMBER,
    UNIVERSAL_TARGET_PARAMETERS_MEMBER,
];

/// Why an invocation timeout `invoke_configuration` states is outside the range
/// its own member documents, or `None` when every one of them is inside it.
///
/// A value that is an EXPRESSION is not read: what it produces is not known
/// until an event is evaluated over it, so the bound is asked of the literals
/// alone.
#[must_use]
pub fn invocation_timeout_fault(invoke_configuration: &InvokeConfiguration) -> Option<String> {
    configured_values(invoke_configuration)
        .into_iter()
        .find_map(|(path, value)| {
            let (block, member) = path.split_once('.')?;
            if member != INVOCATION_TIMEOUT_MEMBER || !BOUNDED_TIMEOUT_BLOCKS.contains(&block) {
                return None;
            }
            if super::is_expression(value) {
                return None;
            }
            let seconds = value.trim().parse::<i64>().ok().filter(|seconds| {
                (MIN_INVOCATION_TIMEOUT_SECONDS..=MAX_INVOCATION_TIMEOUT_SECONDS)
                    .contains(seconds)
            });
            seconds
                .is_none()
                .then(|| INVOCATION_TIMEOUT_MESSAGE.to_string())
        })
}

/// Why the target `invoke_configuration` names belongs to another account, or
/// `None` when it is one this caller's subscriber may be pointed at.
///
/// A subscriber is minted in the calling account — it is created on a bus of
/// that account and delivers under a role of it ("Must belong to the calling
/// account") — so a target ARN naming a different account names a resource the
/// subscriber cannot be given. Only an account ID names an account
/// ([`::service_common::arn::is_account_id`]): the universal target form
/// (`arn:aws:events:::aws-sdk:…`) carries an EMPTY account field, and the
/// resource-neutral `TargetResourceArn` grammar admits any text there, so a
/// value that is not twelve digits names no OTHER account and is left to the
/// delivery, which reaches no resource for it.
///
/// The `Some` is the whole `Message` of the refusal, whose variable parts are the
/// account the request's own target ARN names and the account the subscriber
/// would be created in.
#[must_use]
pub fn target_account_fault(invoke_configuration: &InvokeConfiguration) -> Option<String> {
    let parsed = ::service_common::arn::parse(&invoke_configuration.target_arn)?;
    let caller = ::engine::emulator_account_id();
    if !::service_common::arn::is_account_id(parsed.account) || parsed.account == caller {
        return None;
    }
    Some(format!(
        "Target resource account ({}) must match the subscriber account ({caller})",
        parsed.account
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        target_account_fault, target_parameters_fault, target_parameters_member,
        EVENT_BUS_V2_PARAMETERS_MEMBER, HTTP_PARAMETERS_MEMBER,
        KINESIS_PARAMETERS_MEMBER, LAMBDA_PARAMETERS_MEMBER, SNS_PARAMETERS_MEMBER,
        SQS_PARAMETERS_MEMBER, STEP_FUNCTIONS_PARAMETERS_MEMBER,
        UNIVERSAL_TARGET_PARAMETERS_MEMBER, NO_TARGET_PARAMETERS,
    };
    use crate::ops::types::{
        EventBusV2Parameters, InvokeConfiguration, LambdaParameters, SqsParameters,
    };

    /// The account every ARN below is written in, the one a request reaching
    /// this emulator arrives as.
    fn caller() -> String {
        ::engine::emulator_account_id().to_string()
    }

    /// RULE: the parameters member a target takes follows from the service its
    /// ARN names, across every member the model declares.
    #[test]
    fn each_target_service_takes_its_own_parameters_member() {
        let account = caller();
        let rows: [(&str, String, Option<&str>); 9] = [
            (
                "a Lambda function",
                format!("arn:aws:lambda:us-west-2:{account}:function:deliver"),
                Some(LAMBDA_PARAMETERS_MEMBER),
            ),
            (
                "an SQS queue",
                format!("arn:aws:sqs:us-west-2:{account}:deliveries"),
                Some(SQS_PARAMETERS_MEMBER),
            ),
            (
                "an SNS topic",
                format!("arn:aws:sns:us-west-2:{account}:deliveries"),
                Some(SNS_PARAMETERS_MEMBER),
            ),
            (
                "a Kinesis stream",
                format!("arn:aws:kinesis:us-west-2:{account}:stream/deliveries"),
                Some(KINESIS_PARAMETERS_MEMBER),
            ),
            (
                "a Step Functions state machine",
                format!("arn:aws:states:us-west-2:{account}:stateMachine:deliveries"),
                Some(STEP_FUNCTIONS_PARAMETERS_MEMBER),
            ),
            (
                "an API Gateway stage",
                format!("arn:aws:execute-api:us-west-2:{account}:abcdef1234/live/POST/events"),
                Some(HTTP_PARAMETERS_MEMBER),
            ),
            (
                "an EventBridge API destination",
                format!("arn:aws:events:us-west-2:{account}:api-destination/partner/abc"),
                Some(HTTP_PARAMETERS_MEMBER),
            ),
            (
                "a universal (aws-sdk) call",
                "arn:aws:events:::aws-sdk:s3:putObject".to_string(),
                Some(UNIVERSAL_TARGET_PARAMETERS_MEMBER),
            ),
            (
                "another event bus",
                format!(
                    "arn:aws:events:us-west-2:{account}:event-busv2/downstream/\
                     abcdefghijklmnopqrstuvwxy"
                ),
                Some(EVENT_BUS_V2_PARAMETERS_MEMBER),
            ),
        ];
        for (case, arn, expected) in rows {
            let read = target_parameters_member(&arn);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?} for {arn}, got {read:?}"
            );
        }
    }

    /// RULE: a member the target does not take is refused, naming the member
    /// supplied, the target and the member the target takes; a member the target
    /// does take, and no member at all, are not faults.
    #[test]
    fn a_parameters_member_the_target_does_not_take_is_a_fault() {
        let account = caller();
        let queue = format!("arn:aws:sqs:us-west-2:{account}:deliveries");
        let mismatched = InvokeConfiguration {
            role_arn: format!("arn:aws:iam::{account}:role/deliver"),
            target_arn: queue.clone(),
            lambda_parameters: Some(LambdaParameters::default()),
            ..Default::default()
        };
        assert_eq!(
            target_parameters_fault(&mismatched),
            Some(format!(
                "{LAMBDA_PARAMETERS_MEMBER} is not supported for target {queue}. \
                 Expected {SQS_PARAMETERS_MEMBER}"
            )),
            "a Lambda parameter block on a queue target must be refused"
        );

        let matched = InvokeConfiguration {
            lambda_parameters: None,
            sqs_parameters: Some(SqsParameters::default()),
            ..mismatched.clone()
        };
        assert_eq!(
            target_parameters_fault(&matched),
            None,
            "the queue's own parameter block must be accepted"
        );

        let bare = InvokeConfiguration {
            lambda_parameters: None,
            ..mismatched
        };
        assert_eq!(
            target_parameters_fault(&bare),
            None,
            "a configuration naming no parameter block must be accepted"
        );

        // A target of a service this one documents no parameters member for:
        // an event bus of the previous generation, whose resource type is
        // `event-bus` rather than this service's `event-busv2`.
        let classic = format!("arn:aws:events:us-west-2:{account}:event-bus/classic");
        let on_classic = InvokeConfiguration {
            role_arn: format!("arn:aws:iam::{account}:role/deliver"),
            target_arn: classic.clone(),
            event_bus_v2_parameters: Some(EventBusV2Parameters::default()),
            ..Default::default()
        };
        assert_eq!(
            target_parameters_fault(&on_classic),
            Some(format!(
                "{EVENT_BUS_V2_PARAMETERS_MEMBER} is not supported for target {classic}. \
                 Expected {NO_TARGET_PARAMETERS}"
            )),
            "a parameter block on a target that takes none must be refused"
        );
        assert_eq!(
            target_parameters_member(&classic),
            None,
            "a previous-generation event bus takes no parameters member"
        );
    }

    /// RULE: the target is read against the calling account.
    #[test]
    fn another_accounts_target_and_role_are_refused() {
        let account = caller();
        let other = "111111111111";
        let foreign = InvokeConfiguration {
            role_arn: format!("arn:aws:iam::{account}:role/deliver"),
            target_arn: format!("arn:aws:sqs:us-west-2:{other}:deliveries"),
            ..Default::default()
        };
        assert_eq!(
            target_account_fault(&foreign),
            Some(format!(
                "Target resource account ({other}) must match the subscriber account ({account})"
            )),
            "a target in another account must be refused"
        );

        let own = InvokeConfiguration {
            target_arn: format!("arn:aws:sqs:us-west-2:{account}:deliveries"),
            ..foreign.clone()
        };
        assert_eq!(
            target_account_fault(&own),
            None,
            "a target in the calling account must be accepted"
        );

        let universal = InvokeConfiguration {
            target_arn: "arn:aws:events:::aws-sdk:s3:putObject".to_string(),
            ..foreign.clone()
        };
        assert_eq!(
            target_account_fault(&universal),
            None,
            "an account-less universal target names no other account"
        );

        let unaccounted = InvokeConfiguration {
            target_arn: "arn:aws:sqs:us-west-2:my-account:deliveries".to_string(),
            ..foreign
        };
        assert_eq!(
            target_account_fault(&unaccounted),
            None,
            "an account field that is no account id names no other account"
        );
    }
}
