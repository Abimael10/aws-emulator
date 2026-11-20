// SPDX-License-Identifier: CC0-1.0

//! The `AWS` integration whose `uri` names AWS LAMBDA's function-invoking action: how a
//! method request is served by INVOKING that function in process, with what the integration
//! request rendered as the event and what the function returned as the backend answer the
//! integration's own responses map.
//!
//! `IntegrationType` gives `AWS` "for integrating the API method request with an AWS service
//! action, INCLUDING THE LAMBDA FUNCTION-INVOKING ACTION", and `Integration$uri` gives the
//! form that action is named in —
//! `arn:aws:apigateway:{region}:{subdomain.service|service}:path|action/{service_api}` — which
//! for that action is the Lambda data-plane path
//! `path/{version}/functions/{function-arn}/invocations`. So the `uri` names ONE function,
//! read through the ONE reading of that path this tree has
//! ([`super::super::lambda_proxy::invoked_function_path`]), and the call is the `Invoke` of it
//! — made under the `InvocationType` the integration's own `requestParameters` map onto the
//! integration request ([`invocation_type`]), which is how a method whose backend is invoked
//! ASYNCHRONOUSLY is configured: the invocation is answered as soon as Lambda has queued it,
//! with no payload of a run, and that answer is mapped by this integration's own responses
//! like every other.
//!
//! What makes this integration the NON-PROXY one is everything around that invocation, and
//! none of it is this module's own invention:
//!
//! * the event the function is handed is the INTEGRATION REQUEST — the request template this
//!   call selects, rendered, or the method request's own payload where no template maps it and
//!   the `passthroughBehavior` lets it through ([`super::integration_request_body`]) — rather
//!   than the proxy event a `AWS_PROXY` integration passes the whole request through as;
//! * what the function returned is the backend answer, with the status Lambda answered the
//!   invocation with (`InvocationResponse$StatusCode` is bound to the response code: "for the
//!   `RequestResponse` invocation type, this status code is 200"), for the integration
//!   response whose `selectionPattern` matches it to map — which is the one step
//!   [`super::super::integration_response`] takes for every backend answer;
//! * a run that ERRORED is still an answer: Lambda reports it with
//!   `InvocationResponse$FunctionError` ("if present, indicates that an error occurred during
//!   function execution. Details about the error are included in the response payload"), and
//!   that error is what `IntegrationResponse$selectionPattern` is matched against for this
//!   backend — "if the back end is an AWS Lambda function, the AWS Lambda function error
//!   header is matched. For all other HTTP and AWS back ends, the HTTP status code is matched"
//!   — so such an answer travels to the mapping step as the error the run REPORTED
//!   ([`reported_error`], the message of the error document, which is what a pattern written
//!   for a class of failure matches) and its payload, the error document, is what a response
//!   template then reads.
//!
//! The invocation is made THROUGH THE EMULATOR this service runs within, over the in-process
//! transport `OperationContext::callback` hands a handler ([`super::super::lambda_proxy`]'s own
//! client): nothing is invented and no connection is opened, and what the method response
//! carries is what the function's own run produced. The wait is the integration's stored
//! `timeoutInMillis` ([`super::super::lambda_proxy::waited_for`]), so a request to a deployed
//! stage is answered whatever the run does with its own time.
//!
//! # Who the call is made as
//!
//! The same choice `Integration$credentials` words for every `AWS` integration, and the same
//! one the proxy type takes:
//!
//! * a `credentials` naming a ROLE has the call made under that role, so the role has to let
//!   API Gateway assume it and its own policies have to allow `lambda:InvokeFunction` on the
//!   function — the rule the PARENT module states for every `AWS` integration, which this
//!   module only names the action ([`Call::iam_action`]), the resource ([`Call::function_arn`])
//!   and the refusal ([`access_denied`]) for;
//! * a `credentials` naming none has it made as API Gateway's own service principal, so what
//!   admits it is the FUNCTION's resource-based policy — the document `AddPermission` writes —
//!   read from Lambda itself through the one reading of that grant this tree has
//!   ([`super::super::lambda_proxy::invocation_admitted`]), with the invoking METHOD's ARN as
//!   the `aws:SourceArn` such a grant is narrowed by.
//!
//! Neither reading refuses on silence: a role this emulator's IAM holds nothing under, a
//! function Lambda answers nothing about, and an emulator with no Lambda or no IAM mounted each
//! leave the call to be made, for the reasons those modules give. A function API Gateway may
//! not invoke must not RUN here, which is why the grant is read BEFORE the invocation; whether
//! the `uri`'s spelling resolves to a record at all is LAMBDA's own answer to the invocation
//! that is then issued.

use std::time::Duration;

use ::smithy_server_runtime::OperationContext;

use crate::ops::types::GetIntegrationOutput;

use super::super::lambda_proxy::{
    invocation_admitted, invocation_refused, invoked_function_path, lambda_client, stated_role,
    waited_for, INVALID_PERMISSIONS_REASON, INVOKE_FUNCTION_ACTION, LAMBDA_SUBDOMAIN,
};
use super::super::lambda_proxy::invocation_type;
use super::super::{MethodRequest, UnservedIntegration, INTEGRATION_TIMEOUT};
use super::{
    bounded, integration_request_body, named_service, substituted, unanswered, Answered, Backend,
    APIGATEWAY_VENDOR,
};

/// What AWS Lambda is called in the sentence an integration that reached no answer carries.
const LAMBDA_SERVICE_NAME: &str = "AWS Lambda";

/// The media type the document an invocation answers with is JSON under: `Invoke` carries the
/// function's own answer as its payload, which is the JSON document the run returned.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The status Lambda answers an invocation it refuses on permissions with, which is the
/// refusal a role that may not make this call earns: the AWS common `AccessDeniedException`,
/// answered by the layer in front of the service's own code, is HTTP 403.
const ACCESS_DENIED_STATUS: u16 = 403;

/// The member that refusal carries its sentence in: every Lambda error structure spells it
/// `Message`, which is what the service's own authorization refusal renders
/// (`::service_com_amazonaws_lambda_awsgirapiservice`'s `AccessDeniedException`).
const MESSAGE_MEMBER: &str = "Message";

/// The resource type a function's ARN names it under, which an identity-based policy grants
/// the invocation on: `arn:{partition}:lambda:{region}:{account}:function:{name}`.
const FUNCTION_RESOURCE_TYPE: &str = "function";

/// The status every `Invoke` of the `RequestResponse` type answers a run it MADE with, which
/// is the status reported when Lambda's own answer carries none a method response can be
/// mapped from: "for the `RequestResponse` invocation type, this status code is 200"
/// (`InvocationResponse$StatusCode`).
const INVOCATION_STATUS: u16 = 200;

/// The member of the error document a failed run answers with that carries the error's own
/// message, which every Lambda runtime reports it under.
const ERROR_MESSAGE_MEMBER: &str = "errorMessage";

/// One invocation an `AWS` integration addresses: the function its `uri` names, the Region of
/// the Lambda endpoint it is made at, who it is made as, how long it waits and the event it
/// carries.
#[derive(Debug)]
pub(super) struct Call {
    /// The partition the integration ARN names, which the function's own ARN carries.
    partition: String,
    /// The Region the integration ARN names, or the api's own when it names none.
    region: String,
    /// The function the `uri` addresses, exactly as that `uri` spelled it — a name, a partial
    /// ARN or a full one, qualified or not — because that is the spelling Lambda itself
    /// resolves.
    function: String,
    /// Whether the integration names a ROLE for this call to be made under, which is what
    /// decides WHICH records admit the invocation (see the module docs).
    assumes_role: bool,
    /// How long the invocation waits for the function's answer: the integration's own
    /// `timeoutInMillis`.
    within: Duration,
    /// WHICH invocation is made: the `InvocationType` the integration's own
    /// `requestParameters` map onto the integration request ([`invocation_type`]), which is
    /// what decides whether the answer this call is served from carries a run at all.
    invocation: ::aws_sdk_lambda::types::InvocationType,
    /// The integration request's body, which IS the event the function is handed.
    body: Vec<u8>,
}

impl Call {
    /// The Region of the Lambda endpoint the invocation is made at.
    pub(super) fn region(&self) -> &str {
        &self.region
    }

    /// The IAM action the invocation is authorized by, which is not the operation's own name:
    /// `Invoke` "requires permission for the `lambda:InvokeFunction` action".
    pub(super) fn iam_action(&self) -> &'static str {
        INVOKE_FUNCTION_ACTION
    }

    /// The ARN of the function the call addresses, which is what an identity-based policy
    /// grants that action on.
    ///
    /// A `uri` that spelled the function by its ARN names it outright — qualifier included,
    /// since a grant may be written on one version or alias. One that spelled a NAME names a
    /// function of this account, in the partition the integration ARN carries and the Region
    /// the call is made at, under the resource type Lambda publishes for one.
    pub(super) fn function_arn(&self) -> String {
        if ::service_common::arn::parse(&self.function)
            .is_some_and(|arn| arn.vendor.eq_ignore_ascii_case(LAMBDA_SUBDOMAIN))
        {
            return self.function.clone();
        }
        format!(
            "arn:{partition}:{LAMBDA_SUBDOMAIN}:{region}:{account}:{FUNCTION_RESOURCE_TYPE}:\
             {function}",
            partition = self.partition,
            region = self.region,
            account = ::engine::emulator_account_id(),
            function = self.function.trim_start_matches(&format!(
                "{FUNCTION_RESOURCE_TYPE}:"
            )),
        )
    }

    /// The INTEGRATION REQUEST this call carries to Lambda, which is what the stage's
    /// execution log reports as the integration request after transformations
    /// ([`super::super::stage_logs`]): the event the invocation is made with, read from the
    /// call itself rather than rendered a second time.
    pub(super) fn sent(&self) -> &[u8] {
        &self.body
    }
}

/// What AWS Lambda answers an invocation the integration's role may not make: the AWS common
/// `AccessDeniedException` envelope — HTTP 403 carrying the sentence IAM words an unauthorized
/// call with, through the tree's one formatter for it
/// ([`::service_common::iam_authorization::not_authorized_by_identity_policy`]) — which names
/// the principal the call would have been made as, the action it asked for, the function it
/// addressed and the clause closing an implicit deny.
///
/// `principal` is the role the call would have been made under.
pub(super) fn access_denied(call: &Call, principal: &str) -> Answered {
    let action = call.iam_action();
    let body = ::serde_json::json!({
        MESSAGE_MEMBER: ::service_common::iam_authorization::not_authorized_by_identity_policy(
            principal,
            action,
            &call.function_arn(),
            &::service_common::iam_authorization::no_identity_policy_allows(action),
        ),
    });
    Answered {
        status: ACCESS_DENIED_STATUS,
        content_type: Some(JSON_CONTENT_TYPE.to_string()),
        body: body.to_string().into_bytes(),
    }
}

/// What `integration`'s `uri` addresses when it names AWS Lambda's function-invoking action:
///
/// * `None` when the `uri` names no Lambda call at all, which leaves it to another reader;
/// * `Some(Err(…))` when it DOES name Lambda and no invocation can be placed from it, which is
///   a configuration this stage cannot serve — or an integration request no template of it
///   maps, which is the one refusal a method request earns here;
/// * `Some(Ok(call))` for the invocation to make.
pub(super) fn addressed(
    integration: &GetIntegrationOutput,
    default_region: &str,
    call: &MethodRequest,
) -> Option<Result<Call, UnservedIntegration>> {
    let uri = integration.uri.as_deref()?;
    let arn = ::service_common::arn::parse(uri)?;
    if arn.vendor != APIGATEWAY_VENDOR
        || !named_service(arn.account).eq_ignore_ascii_case(LAMBDA_SUBDOMAIN)
    {
        return None;
    }
    Some(resolved(integration, &arn, default_region, call))
}

/// The invocation the integration ARN `arn` names, with every `{placeholder}` of its resource
/// resolved against this request and the event the function is handed rendered.
fn resolved(
    integration: &GetIntegrationOutput,
    arn: &::service_common::arn::Arn<'_>,
    default_region: &str,
    call: &MethodRequest,
) -> Result<Call, UnservedIntegration> {
    let region = match arn.region.is_empty() {
        true => default_region,
        false => arn.region,
    };
    let resource = substituted(arn.resource, integration, &call.inputs)
        .map_err(|reason| unanswered(&reason))?;
    // Only the PATH form addresses this action: it is Lambda's own REST path for a
    // synchronous invocation, and the `action` form names no operation of that data plane.
    let Some(function) = invoked_function_path(&resource) else {
        return Err(unanswered(
            "the integration's uri does not address the function-invoking path of \
             AWS Lambda, so it names no function to invoke",
        ));
    };
    let body = integration_request_body(integration, call)?;
    Ok(Call {
        partition: arn.partition.to_string(),
        region: region.to_string(),
        function: function.to_string(),
        assumes_role: stated_role(integration.credentials.as_deref()).is_some(),
        within: waited_for(integration),
        invocation: invocation_type::asked_for(integration, &call.inputs),
        body,
    })
}

/// Make the invocation `call` addresses, in process, and report what the function's run
/// answered.
///
/// `Err` only where there is no answer FROM LAMBDA to report at all — the invocation was not
/// admitted, it was not answered, the run did not finish within the integration's own timeout,
/// or the answer is larger than one method response may carry. Every answer Lambda DID give,
/// a run that errored included, is reported as a backend answer and mapped.
pub(super) async fn answered(
    ctx: &OperationContext<'_>,
    call: &Call,
    request: &MethodRequest,
) -> Result<Backend, UnservedIntegration> {
    let client = lambda_client(ctx, &call.region);
    // Made as API Gateway's own principal, so the FUNCTION's resource-based policy is what
    // admits it. A call that names no method at all leaves a grant's `aws:SourceArn` nothing
    // to be judged against, so there is no decision to take about it.
    if !call.assumes_role {
        if let Some(source_arn) = request.proxy.source_arn.as_deref() {
            if !invocation_admitted(&client, &call.function, source_arn).await {
                return Err(unanswered(INVALID_PERMISSIONS_REASON));
            }
        }
    }
    bounded(call.body.len())?;
    let invocation = client
        .invoke()
        .function_name(&call.function)
        .invocation_type(call.invocation.clone())
        .payload(::aws_sdk_lambda::primitives::Blob::new(call.body.clone()))
        .send();
    let answer = match ::tokio::time::timeout(call.within, invocation).await {
        Ok(answer) => answer,
        // The integration's own timeout fell due with no answer, so the backend did not
        // answer this call: the request is answered through the api's `INTEGRATION_TIMEOUT`
        // gateway response rather than held open for a run that is still going.
        Err(_elapsed) => {
            ::tracing::warn!(
                waited_ms = call.within.as_millis(),
                "the AWS integration's function did not answer within the integration's \
                 timeout"
            );
            return Err(UnservedIntegration {
                refusal: INTEGRATION_TIMEOUT,
                reason: format!(
                    "the AWS integration's function did not answer within the integration's \
                     timeoutInMillis of {}, so there is no backend answer to map",
                    call.within.as_millis()
                ),
            });
        }
    };
    let answered = match answer {
        Ok(answered) => answered,
        Err(error) => {
            ::tracing::warn!(
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the AWS integration's function was not invoked"
            );
            return Err(unanswered(
                // LAMBDA refusing the invocation it was issued — a spelling it holds no record
                // for, a call its own records do not admit — is the answer to whether this
                // function may be invoked at all, so it is the configuration error API Gateway
                // words with its one permissions sentence. Every other outcome is a different
                // story: an invocation nothing answered, or one Lambda answered something else
                // about.
                &if invocation_refused(&error) {
                    INVALID_PERMISSIONS_REASON.to_string()
                } else {
                    format!(
                        "the integration request to {LAMBDA_SERVICE_NAME} was not answered, so \
                         there is no backend answer to map"
                    )
                },
            ));
        }
    };
    let function_error = answered.function_error().map(str::to_string);
    let body = answered
        .payload()
        .map(|payload| payload.as_ref().to_vec())
        .unwrap_or_default();
    bounded(body.len())?;    // The status Lambda answered the invocation with, which is the one its own `StatusCode`
    // reports; an answer that reports none outside the range an HTTP response carries is read
    // as the status that invocation type is answered with.
    let status = u16::try_from(answered.status_code())
        .ok()
        .filter(|status| (100..600).contains(status))
        .unwrap_or(INVOCATION_STATUS);
    Ok(Backend {
        answered: Answered {
            status,
            content_type: Some(JSON_CONTENT_TYPE.to_string()),
            body: body.clone(),
        },
        // A run that errored is matched against the error it REPORTED rather than against the
        // status Lambda answered the invocation with ([`reported_error`]).
        function_error: function_error.map(|signal| reported_error(&body, signal)),
    })
}

/// What the `selectionPattern` of this back end is matched against for a run that ERRORED:
/// the error the run reported, out of the error document Lambda answered with
/// ([`ERROR_MESSAGE_MEMBER`]) — "details about the error are included in the response payload"
/// (`InvocationResponse$FunctionError`) — and the function-error SIGNAL itself where that
/// payload carries no message of its own, so an answer is always matched against something the
/// run reported rather than against nothing.
fn reported_error(body: &[u8], signal: String) -> String {
    ::serde_json::from_slice::<::serde_json::Value>(body)
        .ok()
        .as_ref()
        .and_then(|document| document.get(ERROR_MESSAGE_MEMBER))
        .and_then(::serde_json::Value::as_str)
        .filter(|message| !message.is_empty())
        .map(str::to_string)
        .unwrap_or(signal)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Call` of `function`, as [`resolved`] builds one for a `uri` naming it.
    fn call(function: &str, region: &str) -> Call {
        Call {
            partition: "aws".to_string(),
            region: region.to_string(),
            function: function.to_string(),
            assumes_role: true,
            within: Duration::from_millis(29_000),
            invocation: ::aws_sdk_lambda::types::InvocationType::RequestResponse,
            body: Vec::new(),
        }
    }

    /// RULE: the invocation is authorized as `lambda:InvokeFunction` on the function the `uri`
    /// names — the ARN it spelled, or the ARN of the function of this account the name it
    /// spelled stands for.
    #[test]
    fn the_invocation_is_authorized_on_the_function_the_uri_names() {
        /// (case, the function the `uri` spelled, the Region of the call, the resource)
        type Row<'a> = (&'a str, &'a str, &'a str, &'a str);
        let rows: &[Row<'_>] = &[
            (
                "a full function ARN",
                "arn:aws:lambda:us-east-1:000000000000:function:echo",
                "us-east-1",
                "arn:aws:lambda:us-east-1:000000000000:function:echo",
            ),
            (
                "a function ARN qualified by an alias, which a grant may be written on",
                "arn:aws:lambda:eu-west-1:000000000000:function:echo:live",
                "eu-west-1",
                "arn:aws:lambda:eu-west-1:000000000000:function:echo:live",
            ),
            (
                "a bare function name, which names a function of this account at the Region \
                 the call is made at",
                "echo",
                "eu-west-1",
                "arn:aws:lambda:eu-west-1:000000000000:function:echo",
            ),
            (
                "a bare name qualified by a version",
                "echo:3",
                "us-east-2",
                "arn:aws:lambda:us-east-2:000000000000:function:echo:3",
            ),
            (
                "a name already spelled under the resource type, which is not doubled",
                "function:echo",
                "us-east-1",
                "arn:aws:lambda:us-east-1:000000000000:function:echo",
            ),
        ];
        for (case, function, region, expected) in rows {
            let call = call(function, region);
            assert_eq!(
                call.iam_action(),
                "lambda:InvokeFunction",
                "case {case:?}: expected the invoking action, got {:?}",
                call.iam_action()
            );
            assert_eq!(
                call.function_arn(),
                *expected,
                "case {case:?}: expected the resource {expected:?}, got {:?}",
                call.function_arn()
            );
        }
    }

    /// RULE: a refusal of the integration's role is the one AWS Lambda answers an unauthorized
    /// call with — HTTP 403 and the sentence IAM words it in, naming the principal, the action
    /// and the function.
    #[test]
    fn an_unauthorized_invocation_is_refused_the_way_lambda_words_it() {        const ROLE: &str = "arn:aws:iam::000000000000:role/apigateway-integration";
        let function = "arn:aws:lambda:us-east-1:000000000000:function:echo";
        let refused = access_denied(&call(function, "us-east-1"), ROLE);
        assert_eq!(
            refused.status, ACCESS_DENIED_STATUS,
            "expected the status Lambda refuses an unauthorized call with, got {}",
            refused.status
        );
        let document: ::serde_json::Value = ::serde_json::from_slice(&refused.body)
            .expect("the refusal carries Lambda's own JSON envelope");
        let message = document[MESSAGE_MEMBER].as_str().unwrap_or_default();
        assert_eq!(
            message,
            format!(
                "User: {ROLE} is not authorized to perform: {INVOKE_FUNCTION_ACTION} on \
                 resource: {function} because no identity-based policy allows the \
                 {INVOKE_FUNCTION_ACTION} action"
            ),
            "expected the sentence IAM words an authorization failure in, got {message:?}"
        );
    }

    /// RULE: a run that errored is matched against the error it REPORTED — the message of the
    /// error document — and against the function-error signal only where that document carries
    /// no message at all.
    #[test]
    fn a_failed_run_is_matched_against_the_error_it_reported() {
        /// (case, the answer's payload, the function-error signal, the subject)
        type Row<'a> = (&'a str, &'a str, &'a str, &'a str);
        let rows: &[Row<'_>] = &[
            (
                "an error document naming the message the handler raised",
                r#"{"errorType":"ValueError","errorMessage":"Malformed order","trace":[]}"#,
                "Unhandled",
                "Malformed order",
            ),
            (
                "an error document naming a class the platform reported",
                r#"{"errorType":"Runtime.OutOfMemory","errorMessage":"out of memory"}"#,
                "Unhandled.OutOfMemory",
                "out of memory",
            ),
            (
                "an error document carrying an empty message, which reports none",
                r#"{"errorType":"ValueError","errorMessage":""}"#,
                "Unhandled",
                "Unhandled",
            ),
            (
                "an error document carrying no message member",
                r#"{"errorType":"ValueError"}"#,
                "Unhandled",
                "Unhandled",
            ),
            ("a payload that is no document at all", "not json", "Unhandled", "Unhandled"),
            ("no payload at all", "", "Unhandled", "Unhandled"),
        ];
        for (case, payload, signal, expected) in rows {
            let actual = reported_error(payload.as_bytes(), (*signal).to_string());
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected the subject {expected:?}, got {actual:?}"
            );
        }
    }
}
