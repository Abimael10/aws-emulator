// SPDX-License-Identifier: CC0-1.0

//! Delivering a matched event to an AWS AppSync GRAPHQL API target: the GraphQL operation
//! EventBridge executes on the API the target's ARN names, carrying what the target
//! receives as that operation's variables.
//!
//! A target of the `appsync` service names the API's GraphQL ENDPOINT —
//! `arn:{partition}:appsync:{region}:{account}:endpoints/graphql-api/{label}`, the endpoint
//! AWS publishes at `{label}.appsync-api.{region}.amazonaws.com` — or the API itself, and
//! optionally one field of it, by id (`apis/{apiId}`, `apis/{apiId}/types/{typeName}/fields/{fieldName}`,
//! the form the `appsync:GraphQL` action is granted on). The operation to run on it is the
//! target's own: "the GraphQL operation; that is, the query, mutation, or subscription to be
//! parsed and executed by the GraphQL service"
//! (`Target$AppSyncParameters.GraphQLOperation`). What the target receives is that
//! operation's VARIABLES: a document is a template with named variables, and the values that
//! fill them are what the event is reshaped into — "Input: valid JSON text passed to the
//! target", an `InputPath`'s selection or an `InputTransformer`'s rendering
//! ([`super::transformed`]) — so a target delivering one field of an event states the
//! variables that field fills in its own template and nothing else travels.
//!
//! EventBridge invokes an AppSync target AS THE ROLE the target names, and the call it makes
//! under it is `appsync:GraphQL` on the FIELD the operation selects — which is what an IAM
//! policy about an API's fields is written over, whichever way the target's own ARN names the
//! API ([`action_resource`] resolves it through the API itself). So that role's trust and its
//! permissions decide the delivery before any operation is executed ([`super::deliver`] asks
//! both, through the shared decision every target kind invoked under its role takes), and a
//! role granted the action on ANOTHER field of the same API delivers nothing.
//!
//! The operation is executed by the API ITSELF. An API's `GRAPHQL` endpoint is a socket that
//! service serves per API rather than a route of this emulator's front door, so there is no
//! address to make a request at: the request is handed to the very code that endpoint serves
//! (`service::graphql_api::in_process`), in process, the way a Glue workflow target's event
//! is handed to Glue's own store. It carries the SIGNATURE every in-process call of this
//! emulator carries ([`::service_common::in_process_sigv4`]), under the signing name AppSync
//! publishes its data plane under, because an API whose authorization mode is `AWS_IAM`
//! admits no unsigned caller.
//!
//! What the delivery comes to is read off the ANSWER that endpoint gave, and ONLY off it:
//!
//! * a 2xx is the delivery. A GraphQL answer is a 200 whether or not every field of it
//!   resolved — a resolver that raises is reported as an error of its path inside a 200 — so
//!   an answer carrying resolver errors is an event the target ACCEPTED, counted on the
//!   rule's `Invocations` and dead-lettered nowhere: the operation was executed, and what it
//!   resolved to is a fact about the API rather than about the delivery;
//! * a `401` or a `403` is the identity the operation was executed under not being permitted
//!   to perform [`GRAPHQL_ACTION`] on the field, which is [`crate::state::NO_PERMISSIONS`] —
//!   the class a role that does not grant the action earns as well, reported as the answer
//!   the endpoint gives such an identity ([`invoke_authorization_refusal`]);
//! * every other non-2xx answer is the endpoint not serving the operation, which is
//!   [`crate::state::CONNECTION_FAILURE`] — the class an HTTP-endpoint target's unsuccessful
//!   answer earns ([`super::rest_api_stage`] states it for an API Gateway stage). The `500`
//!   that endpoint answers for a request its own panic containment caught is one of them, so
//!   a fault inside the API is a failed delivery here rather than something that unwinds
//!   into this pass.
//!
//! The execution is ONE attempt: it is an in-process handover to the API's own endpoint
//! rather than a request over a connection, so there is no attempt that could be lost on the
//! way and nothing for a retry to reach that this one did not. What the dead letter reports
//! is the answer itself ([`answer_failure`]).
//!
//! Two bounds hold the handover, because a resolver of the API may publish onto a bus of
//! this same emulator and that bus's rules may deliver to that API again: one execution is
//! capped at [`EXECUTION_TIMEOUT`], and a CHAIN of handovers is followed
//! [`MAX_NESTED_HANDOVERS`] deep and no further — with the handovers in flight counted as
//! the backstop ([`crate::state::graphql_handovers`]), which keeps such a cycle finite
//! however it is awaited.

use super::*;

/// The ARN namespace of AWS AppSync, whose API fields a rule delivers to by executing a
/// GraphQL operation on them.
pub(super) const APPSYNC_ARN_NAMESPACE: &str = "appsync";

/// The IAM action the delivery performs, which the target's role has to be allowed on the
/// field the operation selects: the one action AppSync publishes for its data plane.
pub(super) const GRAPHQL_ACTION: &str = "appsync:GraphQL";

/// The resource type an AppSync ENDPOINT's ARN names, and the kind of endpoint a GraphQL
/// API's is: `endpoints/graphql-api/{label}`.
const ENDPOINTS_RESOURCE_TYPE: &str = "endpoints";
const GRAPHQL_API_ENDPOINT: &str = "graphql-api";

/// The resource type an AppSync API's own ARN names, which every field of it is addressed
/// under.
const APIS_RESOURCE_TYPE: &str = "apis";

/// The two segments that name one FIELD within an API: `…/types/{typeName}/fields/{fieldName}`.
const TYPES_SEGMENT: &str = "types";
const FIELDS_SEGMENT: &str = "fields";

/// The service name AWS AppSync's own error metadata carries, which a dead letter reporting
/// that service's answer names it by.
const APPSYNC_SERVICE_NAME: &str = "AWSAppSync";

/// The content type a GraphQL request carries, which is what an endpoint reads its document
/// as.
const GRAPHQL_CONTENT_TYPE: &str = "application/json";

/// The two statuses an AppSync endpoint answers a caller it has not authorized: the call
/// presented no material the API's modes accept, or the identity it signed as may not
/// perform [`GRAPHQL_ACTION`] on a field it selected.
const UNAUTHORIZED_STATUS: u16 = 401;
const FORBIDDEN_STATUS: u16 = 403;

/// The field of a GraphQL answer carrying what did not resolve, and the member of one such
/// entry naming its CLASS — which is what a dead letter reports as the error code of the
/// answer.
const ERRORS_FIELD: &str = "errors";
const ERROR_TYPE_MEMBER: &str = "errorType";

/// The time ONE execution of a target's operation is given.
///
/// This emulator's OWN bound rather than a limit AWS publishes, and the same cap an attempt
/// at an api destination or at an API Gateway stage is given ([`API_DESTINATION_TIMEOUT`]),
/// so one delivery cannot hold the publisher longer than another. It is a CAP rather than a
/// wait: an execution that answers sooner costs what it costs. The execution is bounded from
/// inside as well — the API's own endpoint bounds every template and every engine run it
/// makes — so this is what keeps the DELIVERY interruptible rather than what bounds the
/// work.
const EXECUTION_TIMEOUT: ::std::time::Duration = API_DESTINATION_TIMEOUT;

/// How many handovers deep this emulator follows a CHAIN of them: a delivery executes an
/// operation whose resolvers may publish onto a bus of this same emulator, whose rules may
/// deliver to an API again.
///
/// This emulator's OWN bound rather than a limit AWS publishes, and the counterpart of the
/// one a bus-to-bus forward is held to ([`super::MAX_BUS_HOPS`]): a chain like that is a
/// client's own topology rather than a fault, so it is FOLLOWED, and what this emulator
/// cannot do is follow one without end, because every hop of it is a frame on one worker.
const MAX_NESTED_HANDOVERS: usize = 4;

::tokio::task_local! {
    /// How many handovers of the chain this one belongs to have already been made, which is
    /// what [`MAX_NESTED_HANDOVERS`] bounds.
    ///
    /// Carried on the TASK rather than in a store, because it is a fact about one chain and
    /// not about the emulator: every handover a chain makes is awaited inside the scope of
    /// the one before it ([`execute_graphql_operation`]), so a delivery of another chain —
    /// another publisher's request, a replay, a schedule tick — reads nothing of it and is
    /// never refused for it. A hop that moved a chain onto another task would restore the
    /// count, which is what the handovers-in-flight backstop is for
    /// ([`crate::state::graphql_handovers`]).
    static NESTING: usize;
}

/// What a dead letter reports for a delivery whose target carries no operation to execute.
///
/// Fixed text with no variable part: the target's own parameters are what state the
/// operation, and a dead letter carries the event rather than the target.
const NO_GRAPHQL_OPERATION: &str =
    "The AppSyncParameters of this target state no GraphQLOperation to execute.";

/// What an AppSync target's ARN names.
pub(super) enum Addressed {
    /// The API's GraphQL ENDPOINT, by the label the address that endpoint is published at
    /// carries: which API that is, is AppSync's own fact
    /// (`service::graphql_api::in_process::api_at_graphql_endpoint`).
    Endpoint(String),
    /// The API by its id, and one field of it where the ARN names one — the form an IAM
    /// policy about the API's fields is written over.
    Api {
        api_id: String,
        /// `(typeName, fieldName)`, for an ARN that names one field.
        field: Option<(String, String)>,
    },
}

/// What `arn` names, or `None` when it names no GraphQL API of AppSync — in which case there
/// is no operation to execute and nothing is invented in its place.
///
/// The two forms are the one an EventBridge target carries (the API's published endpoint) and
/// the one the `appsync:GraphQL` action is granted on (the API, and one field of it): each
/// segment has to name something, so an ARN stating fewer of those names — an endpoint with
/// no label, a type with no field — addresses no API, and one stating more addresses nothing
/// of it.
pub(super) fn addressed(arn: &str) -> Option<Addressed> {
    let parsed = ::service_common::arn::parse(arn)?;
    if parsed.vendor != APPSYNC_ARN_NAMESPACE {
        return None;
    }
    let mut segments = parsed.resource.split('/');
    let named = |segment: Option<&str>, expected: &str| segment == Some(expected);
    let names_something = |segment: &str| !segment.is_empty();
    match segments.next()? {
        ENDPOINTS_RESOURCE_TYPE => {
            if !named(segments.next(), GRAPHQL_API_ENDPOINT) {
                return None;
            }
            let label = segments.next().filter(|label| names_something(label))?;
            if segments.next().is_some() {
                return None;
            }
            Some(Addressed::Endpoint(label.to_string()))
        }
        APIS_RESOURCE_TYPE => {
            let api_id = segments.next().filter(|id| names_something(id))?;
            let field = match segments.next() {
                None => None,
                Some(TYPES_SEGMENT) => {
                    let type_name = segments.next().filter(|name| names_something(name))?;
                    if !named(segments.next(), FIELDS_SEGMENT) {
                        return None;
                    }
                    let field_name = segments.next().filter(|name| names_something(name))?;
                    Some((type_name.to_string(), field_name.to_string()))
                }
                Some(_) => return None,
            };
            if segments.next().is_some() {
                return None;
            }
            Some(Addressed::Api {
                api_id: api_id.to_string(),
                field,
            })
        }
        _ => None,
    }
}

/// The RESOURCES the `appsync:GraphQL` of this delivery is performed on, as an IAM policy
/// about an API's fields is written over them: the API's own ARN extended with the type and
/// the field for EVERY root field the target's operation selects.
///
/// Resolved through the API itself (`service::graphql_api::in_process`), because the fields
/// are what the DOCUMENT selects and the types they belong to are what the API's schema binds
/// as that operation's root — neither is in the target. EMPTY leaves the decision to be taken
/// over the target's own ARN ([`TargetKind::action_resource`]): an ARN that already names a
/// field is that resource itself, and for every other case there is no field a statement
/// could be about — a target naming an API this account does not hold, or carrying a document
/// that selects nothing of it.
pub(super) fn action_resources(stores: &Stores<'_>, handover: &Handover<'_>) -> Vec<String> {
    use ::service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::service::graphql_api::in_process;

    let Some(state) = stores.appsync else {
        return Vec::new();
    };
    let target = handover.target;
    let Some(addressed) = addressed(&target.arn) else {
        return Vec::new();
    };
    // An ARN that names the field states the resource itself; nothing is resolved for it.
    if let Addressed::Api {
        field: Some(_), ..
    } = addressed
    {
        return Vec::new();
    }
    let (Some(api_id), Some(operation)) = (
        api_id_of(state, &addressed),
        target.graphql_operation.as_deref(),
    ) else {
        return Vec::new();
    };
    in_process::addressed_field_resources(state, &api_id, operation, &::serde_json::Map::new())
}

/// The id of the API `addressed` names, as AppSync's own records resolve it.
fn api_id_of(
    state: &::engine::ServiceState<crate::state::AppSyncState>,
    addressed: &Addressed,
) -> Option<String> {
    use ::service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::service::graphql_api::in_process;

    match addressed {
        Addressed::Endpoint(label) => in_process::api_at_graphql_endpoint(state, label),
        Addressed::Api { api_id, .. } => Some(api_id.clone()),
    }
}

/// Execute the target's GraphQL operation on the API its ARN names, with `body` as the
/// operation's variables, and answer what came of it.
///
/// The classification is the one the module docs state; the outcome is what
/// [`super::deliver`] records and dead-letters.
pub(super) async fn execute_graphql_operation(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    stores: &Stores<'_>,
    region: &str,
    handover: &Handover<'_>,
    body: &str,
) -> crate::state::InvocationOutcome {
    use ::service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice as appsync;

    let target = handover.target;
    let Some(addressed) = addressed(&target.arn) else {
        return crate::state::InvocationOutcome::NotAttempted(
            "the target ARN names no GraphQL API to execute an operation on".to_string(),
        );
    };
    let Some(operation) = target
        .graphql_operation
        .as_deref()
        .filter(|operation| !operation.trim().is_empty())
    else {
        return crate::state::InvocationOutcome::Failed {
            code: crate::state::INVALID_PARAMETER.to_string(),
            message: NO_GRAPHQL_OPERATION.to_string(),
        };
    };
    // The variables the operation is executed with: what the target receives, which has to
    // be the OBJECT a GraphQL request's `variables` is. A text that is not one is an input
    // this target may not be handed, which is the refusal every delivery whose input is not
    // a document earns ([`super::invalid_input_refusal`]).
    let Some(variables) = ::serde_json::from_str::<::serde_json::Value>(body)
        .ok()
        .and_then(|input| match input {
            ::serde_json::Value::Object(variables) => Some(variables),
            _ => None,
        })
    else {
        return crate::state::InvocationOutcome::Failed {
            code: crate::state::INVALID_JSON.to_string(),
            message: crate::state::INPUT_IS_NOT_JSON.to_string(),
        };
    };
    let Some(state) = stores.appsync else {
        return crate::state::InvocationOutcome::NotAttempted(
            "this emulator carries no AppSync store to execute the operation against"
                .to_string(),
        );
    };
    // How many handovers of this chain are already being made: a resolver of the API may
    // publish onto a bus of this same emulator, whose rules may deliver to that API again,
    // and nothing of the framework's own callback-recursion stamp travels with an in-process
    // handover ([`crate::state::graphql_handovers`] states why). This one is the next of that
    // chain, and a chain longer than the emulator follows ends here: a bound of this
    // emulator's own rather than a refusal AWS produces, so nothing is handed over and
    // nothing is invented in its place.
    let nesting = NESTING.try_with(|nesting| *nesting).unwrap_or(0);
    if nesting >= MAX_NESTED_HANDOVERS {
        ::tracing::warn!(
            target = %target.arn,
            bound = MAX_NESTED_HANDOVERS,
            "a chain of deliveries has executed as many nested GraphQL operations as this \
             emulator follows; this delivery is not made"
        );
        return crate::state::InvocationOutcome::NotAttempted(format!(
            "this emulator follows a chain of {MAX_NESTED_HANDOVERS} nested GraphQL \
             operations, and this delivery would be the next of one"
        ));
    }
    // ...and one place per handover BESIDE that, held until this one is answered: the chain's
    // own count is of this task, and a hop that moves the chain onto another task would
    // restore it, so the handovers in flight are counted as the backstop that keeps such a
    // cycle finite whatever it is awaited on.
    let place = stores
        .events
        .and_then(crate::state::graphql_handovers::HandoverPlace::taken);
    if place.is_none() && stores.events.is_some() {
        ::tracing::warn!(
            target = %target.arn,
            bound = crate::state::graphql_handovers::MAX_IN_FLIGHT_HANDOVERS,
            "as many GraphQL operations are being executed as this emulator executes at \
             once; this delivery is not made"
        );
        return crate::state::InvocationOutcome::NotAttempted(format!(
            "this emulator executes at most {} GraphQL operations at once, and as many are \
             being executed",
            crate::state::graphql_handovers::MAX_IN_FLIGHT_HANDOVERS
        ));
    }
    // The API is not THERE when its own records resolve no API for what the target names:
    // this account holds no such API, a delete took it and its endpoints with it, or the
    // endpoint label names no one API. The target could not be addressed at all, which is the
    // class of a delivery whose resource is missing rather than one its identity was refused.
    let missing = || crate::state::InvocationOutcome::Failed {
        code: crate::state::NO_RESOURCE.to_string(),
        message: appsync::state::API_NOT_FOUND_MESSAGE.to_string(),
    };
    let Some(api_id) = api_id_of(state, &addressed) else {
        return missing();
    };
    let document = ::serde_json::json!({
        "query": operation,
        "variables": ::serde_json::Value::Object(variables),
    });
    let mut request = match ::http::Request::builder()
        .method(::http::Method::POST)
        .uri(format!(
            "{}{}",
            stores.endpoint.trim_end_matches('/'),
            appsync::service::graphql_api::query::GRAPHQL_PATH
        ))
        .header(::http::header::CONTENT_TYPE, GRAPHQL_CONTENT_TYPE)
        .body(document.to_string().into_bytes())
    {
        Ok(request) => request,
        Err(error) => {
            return crate::state::InvocationOutcome::NotAttempted(format!(
                "the GraphQL request this delivery would execute could not be built ({error})"
            ))
        }
    };
    // Signed as this emulator's own in-process caller, under the signing name AppSync
    // publishes its data plane under: an API in the `AWS_IAM` authorization mode serves no
    // unsigned call, so an unsigned handover would be refused by the endpoint rather than
    // executed. What the ROLE the target names may do was decided before this
    // ([`super::deliver`]).
    if let Err(error) = ::service_common::in_process_sigv4::sign_callback_request(
        &mut request,
        APPSYNC_ARN_NAMESPACE,
        region,
        callback,
    )
    .await
    {
        return crate::state::InvocationOutcome::NotAttempted(format!(
            "the GraphQL request this delivery would execute could not be signed ({error})"
        ));
    }
    let executed = ::tokio::time::timeout(
        EXECUTION_TIMEOUT,
        // The chain this handover belongs to, one longer: every handover the operation's own
        // resolvers go on to make is made inside this scope, so each of them reads the depth
        // it really sits at.
        NESTING.scope(
            nesting.saturating_add(1),
            appsync::service::graphql_api::in_process::served(state, &api_id, request),
        ),
    )
    .await;
    let answered = match executed {
        // Nothing answered inside the time one execution is given: there is no status to read
        // a condition off, so this is the endpoint not serving the operation — the class an
        // HTTP-endpoint target's unanswered attempt earns.
        Err(_elapsed) => {
            ::tracing::warn!(
                target = %target.arn,
                seconds = EXECUTION_TIMEOUT.as_secs(),
                "a GraphQL operation this delivery executed was not answered within the time \
                 one execution is given"
            );
            return crate::state::InvocationOutcome::Failed {
                code: crate::state::CONNECTION_FAILURE.to_string(),
                message: unanswered_failure(&target.arn),
            };
        }
        Ok(None) => return missing(),
        Ok(Some(answered)) => answered,
    };
    let status = answered.status().as_u16();
    if answered.status().is_success() {
        return crate::state::InvocationOutcome::Invoked;
    }
    crate::state::InvocationOutcome::Failed {
        code: answer_condition(status).to_string(),
        message: answer_failure(&target.arn, status, &answered),
    }
}

/// The condition EventBridge names on the dead letter of a delivery the API's endpoint
/// answered `status` to.
///
/// The ANSWER is what it is read off: the two statuses AppSync answers a caller it has not
/// authorized are a delivery that failed for want of a permission
/// ([`crate::state::NO_PERMISSIONS`]), and every other unsuccessful status is the endpoint
/// not serving the operation ([`crate::state::CONNECTION_FAILURE`]).
fn answer_condition(status: u16) -> &'static str {
    match status {
        UNAUTHORIZED_STATUS | FORBIDDEN_STATUS => crate::state::NO_PERMISSIONS,
        _ => crate::state::CONNECTION_FAILURE,
    }
}

/// What a dead letter reports for a delivery whose API ANSWERED and did not take the event:
/// the status it answered, the target the operation was executed on, and the error metadata
/// block an AWS answer carries.
///
/// Every variable part is read off this delivery and its answer rather than written here:
/// the target's own ARN, the status the endpoint answered, the `errorType` that answer
/// classified the failure as (falling back to the HTTP status's own reason phrase where the
/// answer carries no error entry) and the id the endpoint named the request it answered by —
/// so what the message states is what the endpoint answered, and no value of one delivery is
/// part of the template.
fn answer_failure(arn: &str, status: u16, answered: &::http::Response<::bytes::Bytes>) -> String {
    use ::service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::service::graphql_api::query::REQUEST_ID_FIELD;

    let reason = ::http::StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or_default();
    let error_code = ::serde_json::from_slice::<::serde_json::Value>(answered.body())
        .ok()
        .as_ref()
        .and_then(|answer| answer.get(ERRORS_FIELD))
        .and_then(|errors| errors.get(0))
        .and_then(|error| error.get(ERROR_TYPE_MEMBER))
        .and_then(|error_type| error_type.as_str())
        .map_or_else(|| reason.to_string(), str::to_string);
    let request_id = answered
        .headers()
        .get(REQUEST_ID_FIELD)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    status_failure(arn, status, &error_code, request_id)
}

/// Why the documents in force on the target's role bar this delivery — worded as the ANSWER
/// the request such a role makes is given — or `None` when nothing of the kind can be shown.
///
/// Why the documents in force on the target's role bar this delivery — worded as the ANSWER
/// the request such a role makes is given — or `None` when nothing of the kind can be shown.
///
/// The decision is the shared one every target kind takes about the call it makes
/// ([`super::role_refusal`]), asked over EVERY field the operation selects
/// ([`action_resources`]): a call reaches each of them, so a role granted the action on one
/// field of a document that selects two may not make it, and the FIRST field the documents
/// withhold is what refuses the delivery. Nothing is refused on evidence this emulator does
/// not have. What is this module's own is the REPORT. An identity that may not perform that
/// action is answered [`UNAUTHORIZED_STATUS`] by the API's own endpoint, so such a delivery
/// fails the way every answered attempt does: the condition that status carries
/// ([`answer_condition`]) and the sentence wording it ([`status_failure`]), under AppSync's
/// own error type for a call it has not authorized. A refusal of the step BEFORE the call — a
/// role EventBridge cannot assume at all — is reported as itself, because no operation is
/// executed at all then.
pub(super) fn invoke_authorization_refusal(
    stores: &Stores<'_>,
    role_arn: Option<&str>,
    handover: &Handover<'_>,
    attempt: &crate::state::DeliveryAttempt<'_>,
) -> Option<(String, String)> {
    use ::service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::service::event_api::UNAUTHORIZED_ERROR_TYPE;

    // The fields of this operation beside the one the shared attempt already names, each
    // asked as its own attempt: the action, the rule and the target are this delivery's and
    // only the resource differs.
    let resources = action_resources(stores, handover);
    let asked: Vec<crate::state::DeliveryAttempt<'_>> = if resources.is_empty() {
        vec![crate::state::DeliveryAttempt {
            rule: attempt.rule,
            target_id: attempt.target_id,
            action: attempt.action,
            resource: attempt.resource,
        }]
    } else {
        resources
            .iter()
            .map(|resource| crate::state::DeliveryAttempt {
                rule: attempt.rule,
                target_id: attempt.target_id,
                action: attempt.action,
                resource,
            })
            .collect()
    };
    let (code, message) = asked
        .iter()
        .find_map(|one| super::role_refusal(stores.iam, role_arn, handover.rule_arn, one))?;
    if code != crate::state::NO_PERMISSIONS {
        return Some((code, message));
    }
    // The id of the answer: an operation that WAS executed carries the id the endpoint named
    // the request it answered by, and this one carries the id minted for the answer it stands
    // for, as every refusal this emulator words for an answer does.
    let request_id = ::service_common::query_error_envelope::new_request_id();
    Some((
        answer_condition(UNAUTHORIZED_STATUS).to_string(),
        status_failure(
            &handover.target.arn,
            UNAUTHORIZED_STATUS,
            UNAUTHORIZED_ERROR_TYPE,
            &request_id,
        ),
    ))
}

/// The sentence a dead letter of this class carries: the status the API's endpoint answered,
/// the target the operation was addressed to, and the error metadata block an AWS answer
/// carries — the same form this service reports every HTTP-endpoint target's answer in
/// ([`super::rest_api_stage`]).
fn status_failure(arn: &str, status: u16, error_code: &str, request_id: &str) -> String {
    format!(
        "Got status code \"{status}\" while trying to invoke \"{arn}\". \
         (Service: {APPSYNC_SERVICE_NAME}; Status Code: {status}; Error Code: {error_code}; \
         Request ID: {request_id}; Proxy: null)"
    )
}

/// What a dead letter reports for an execution NOTHING answered within the time one is given:
/// the target it was addressed to and that bound. The variable parts are the target's own ARN
/// and the bound itself.
fn unanswered_failure(arn: &str) -> String {
    format!(
        "Got no answer from \"{arn}\" within {} seconds.",
        EXECUTION_TIMEOUT.as_secs()
    )
}
