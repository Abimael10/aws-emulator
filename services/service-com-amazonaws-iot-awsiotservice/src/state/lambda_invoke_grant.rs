// SPDX-License-Identifier: CC0-1.0

//! Whether a LAMBDA FUNCTION this service would invoke admits that invocation — the one
//! reading of the IoT -> Lambda authorization boundary, shared by every half of this
//! service that calls a function a caller pointed it at.
//!
//! AWS IoT invokes such a function AS ITS OWN SERVICE PRINCIPAL
//! ([`super::SERVICE_PRINCIPAL`]) rather than under a role the resource carries — none of
//! the resources that name one carry a `roleArn` beside it — so what admits the invocation
//! is the FUNCTION's own resource-based policy: the document AWS Lambda's `AddPermission`
//! writes and `GetPolicy` reports, evaluated on
//! [`INVOKE_FUNCTION_ACTION`](super::rules_engine::lambda_action::INVOKE_FUNCTION_ACTION)
//! over that function with the INVOKING RESOURCE's own ARN as the `aws:SourceArn` such a
//! grant is narrowed by. So a permission added for one resource admits that resource's
//! invocations and no other's, a permission naming another service principal admits none,
//! and a function carrying no policy at all — including a function this account does not
//! hold, which carries nothing — admits none either.
//!
//! The policy is read FROM LAMBDA ITSELF rather than held a second time here, so a
//! permission added or removed there takes effect on the next invocation; and the decision
//! is [`::service_common::iam_authorization`], the tree's one evaluation of policy
//! documents, so this service and every other one handed the same document decide alike.
//!
//! What a read establishes is one of three things ([`InvocationGrant`]), and the two that
//! refuse are the two AWS Lambda ANSWERED:
//!
//! * the function is there and its policy admits this service — the invocation is made;
//! * Lambda answered that this account holds no such function, or that the policy it holds
//!   for it admits this service nothing — the invocation is refused;
//! * nothing answered at all (no AWS Lambda is mounted to ask, or the call could not be
//!   dispatched) — nothing is known about the function, so nothing is established here and
//!   each caller decides what to do with an unread grant.
//!
//! The hop is this crate's one construction of the in-process AWS Lambda client
//! ([`super::custom_authorizer::lambda_client`]), whose transport is the emulator's OWN
//! callback ([`smithy_server_runtime::OperationContext::callback`]): it opens no socket,
//! resolves no host name and cannot leave this process, so the function this resolves is
//! one this emulator itself holds. NOTHING of the request reaches the ADDRESS that client
//! is built for either — the endpoint is built from a Region LABEL alone, held to the
//! Region-label grammar ([`::service_common::arn::region_label`]) before it gets there, so
//! a value carrying a host, a port or a path separator is no Region at all and establishes
//! nothing about the function. The function ARN and the invoking resource's ARN travel as
//! request members of a call this emulator serves itself, never as a destination.

use ::service_common::iam_authorization::{resource_decision, Decision, Request};

use super::custom_authorizer::{lambda_client, LAMBDA_ARN_NAMESPACE};
use super::rules_engine::lambda_action::{
    INVOKE_FUNCTION_ACTION, SOURCE_ACCOUNT_KEY, SOURCE_ARN_KEY,
};

/// What reading a function's grant of this service's invocation established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocationGrant {
    /// AWS Lambda holds the function and its resource policy admits this service's
    /// invocation of it.
    Admitted,
    /// AWS Lambda ANSWERED that this account holds no such function, or that the policy it
    /// holds for the function admits this service's invocation nothing.
    Denied,
    /// Nothing answered: there is no AWS Lambda to ask, or the call could not be
    /// dispatched, so nothing about the function was established.
    Unanswered,
}

/// Read what AWS Lambda says about the function `function_arn` for an invocation this
/// service makes for the resource whose ARN is `source_arn`.
///
/// The function is reached through the Region its OWN ARN names, so a resource naming a
/// function in another Region is resolved there; an ARN naming no Region label falls back
/// to the Region the request was signed for, which is held to the same Region-label grammar
/// — a value that is no Region label addresses nothing, so nothing is asked and nothing is
/// established. Both readings go through [`::service_common::arn`], the tree's one ARN
/// parse.
///
/// Call it with this service's state lock RELEASED: nothing here writes this service's
/// state, and holding the lock across the hop would make every other request of this
/// service wait on AWS Lambda.
pub async fn invocation_grant(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    function_arn: &str,
    source_arn: &str,
) -> InvocationGrant {
    let Some(region) = ::service_common::arn::region_of(function_arn, LAMBDA_ARN_NAMESPACE)
        .or_else(|| ::service_common::arn::region_label(request_region))
    else {
        return InvocationGrant::Unanswered;
    };
    let client = lambda_client(ctx, region);
    // The RESOURCE a grant is taken on is the function's own ARN as Lambda reports it, so
    // it is resolved there rather than assumed to be what the request carried — and the
    // read is also the existence question: a function this account does not hold is one no
    // invocation could ever reach.
    let resolved = match client
        .get_function_configuration()
        .function_name(function_arn)
        .send()
        .await
    {
        Ok(reported) => reported
            .function_arn
            .unwrap_or_else(|| function_arn.to_string()),
        Err(error)
            if error.as_service_error().is_some_and(
                ::aws_sdk_lambda::operation::get_function_configuration::GetFunctionConfigurationError::is_resource_not_found_exception,
            ) =>
        {
            return InvocationGrant::Denied;
        }
        Err(error) => {
            ::tracing::warn!(
                function = %function_arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "nothing answered about a Lambda function this service would invoke; its \
                 grant is left unread"
            );
            return InvocationGrant::Unanswered;
        }
    };
    let document = match client.get_policy().function_name(&resolved).send().await {
        Ok(reported) => reported.policy.unwrap_or_default(),
        // A function Lambda reports no resource policy for carries none, which admits this
        // service nothing: that IS an answer about the function.
        Err(error)
            if error.as_service_error().is_some_and(
                ::aws_sdk_lambda::operation::get_policy::GetPolicyError::is_resource_not_found_exception,
            ) =>
        {
            String::new()
        }
        Err(error) => {
            ::tracing::warn!(
                function = %resolved,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "nothing answered about the resource policy of a Lambda function this \
                 service would invoke; its grant is left unread"
            );
            return InvocationGrant::Unanswered;
        }
    };
    // The decision is `service_common::iam_authorization`, the tree's one evaluation of
    // policy documents, so this service and every other one handed the same document
    // decide alike. The keys the invocation would carry are stated, so a grant narrowed to
    // one resource admits that resource's invocations and no other's.
    let principals = [super::SERVICE_PRINCIPAL.to_string()];
    let context = [
        (SOURCE_ARN_KEY.to_string(), source_arn.to_string()),
        (
            SOURCE_ACCOUNT_KEY.to_string(),
            ::engine::emulator_account_id().to_string(),
        ),
    ];
    let request = Request::new(INVOKE_FUNCTION_ACTION, &resolved, &principals)
        .with_context(&context)
        .with_published_keys(&[SOURCE_ARN_KEY, SOURCE_ACCOUNT_KEY]);
    if resource_decision(&document, &request) == Decision::Allowed {
        InvocationGrant::Admitted
    } else {
        InvocationGrant::Denied
    }
}
