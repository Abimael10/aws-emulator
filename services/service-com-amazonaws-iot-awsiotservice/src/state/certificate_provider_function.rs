// SPDX-License-Identifier: CC0-1.0

//! Whether the LAMBDA FUNCTION a certificate provider names is one this service could
//! call to sign a certificate.
//!
//! A certificate provider IS a Lambda function AWS IoT invokes instead of signing a
//! certificate itself: "all API calls to CreateCertificateFromCsr will invoke the
//! certificate provider to create the certificates" (`CreateCertificateProvider`). The
//! invocation is made BY THIS SERVICE PRINCIPAL ([`super::SERVICE_PRINCIPAL`]) rather
//! than under a role the provider carries, so what admits it is the FUNCTION's own
//! resource-based policy — the document AWS Lambda's `AddPermission` writes and
//! `GetPolicy` reports — evaluated on
//! [`INVOKE_FUNCTION_ACTION`](super::rules_engine::lambda_action::INVOKE_FUNCTION_ACTION)
//! over that function with the PROVIDER's own ARN as the `aws:SourceArn` such a grant is
//! narrowed by. That is the same reading the rules engine's `lambda` action takes of the
//! function a topic rule names ([`super::rules_engine::lambda_action`]), because it is the
//! same boundary: IoT invoking a function nobody permitted it to invoke.
//!
//! The function is resolved BEFORE the provider record is written, because a provider
//! reported ready over a function that cannot be called is a provider whose certificate
//! issuance fails later with no diagnosable cause.
//!
//! What a read establishes is one of three things ([`FunctionValidation`]), and the two
//! that refuse are the two AWS Lambda ANSWERED:
//!
//! * the function is there and its policy admits this service — the create goes ahead;
//! * Lambda answered that this account holds no such function, or that the policy it holds
//!   for it admits this service nothing — the create is refused;
//! * nothing answered at all (no AWS Lambda is mounted to ask, or the call could not be
//!   dispatched) — nothing is known about the function, so nothing is refused. This is
//!   where this reading PARTS from the rules engine's: a rule action that cannot read a
//!   grant must not be performed, while a create must not be refused on evidence this
//!   emulator does not have.
//!
//! The hop is this crate's one construction of the in-process AWS Lambda client
//! ([`super::custom_authorizer::lambda_client`]), whose transport is the emulator's OWN
//! callback ([`smithy_server_runtime::OperationContext::callback`]): it opens no socket,
//! resolves no host name and cannot leave this process, so the function this resolves is
//! one this emulator itself holds. NOTHING of the request reaches the ADDRESS that client
//! is built for either — the endpoint is built from a Region LABEL alone, held to the
//! Region-label grammar ([`::service_common::arn::region_label`]) before it gets there, so
//! a value carrying a host, a port or a path separator is no Region at all and establishes
//! nothing about the function. The function ARN and the provider ARN travel as request
//! members of a call this emulator serves itself, never as a destination.

use ::service_common::iam_authorization::{resource_decision, Decision, Request};

use super::custom_authorizer::{lambda_client, LAMBDA_ARN_NAMESPACE};
use super::rules_engine::lambda_action::{
    INVOKE_FUNCTION_ACTION, SOURCE_ACCOUNT_KEY, SOURCE_ARN_KEY,
};

/// What resolving a certificate provider's Lambda function established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionValidation {
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

/// How this service refuses a certificate provider whose Lambda function it could not
/// validate.
///
/// The fixed text is the one AWS IoT words this refusal with, and the single variable part
/// is the `lambdaFunctionArn` the REQUEST named — the one thing the caller has to change.
/// The family's other sentences live beside the record they are about
/// ([`super::certificate_providers`]).
pub fn lambda_validation_denied_message(lambda_function_arn: &str) -> String {
    format!("Access denied during lambda validation: {lambda_function_arn}")
}

/// Resolve the function `lambda_function_arn` for the provider whose ARN is `provider_arn`,
/// and report what AWS Lambda answered about it.
///
/// The function is reached through the Region its OWN ARN names, so a provider naming a
/// function in another Region is validated there; an ARN naming no Region label falls back
/// to the Region the request was signed for, which is held to the same Region-label grammar
/// — a value that is no Region label addresses nothing, so nothing is asked and nothing is
/// established. Both readings go through [`::service_common::arn`], the tree's one ARN
/// parse, exactly as the stream family's object read holds its Region
/// ([`super::streams::absent_stream_file`]).
///
/// Call it with this service's state lock RELEASED: nothing here writes this service's
/// state, and holding the lock across the hop would make every other request of this
/// service wait on AWS Lambda.
pub async fn validate_provider_function(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    provider_arn: &str,
    lambda_function_arn: &str,
) -> FunctionValidation {
    let Some(region) = ::service_common::arn::region_of(lambda_function_arn, LAMBDA_ARN_NAMESPACE)
        .or_else(|| ::service_common::arn::region_label(request_region))
    else {
        return FunctionValidation::Unanswered;
    };
    let client = lambda_client(ctx, region);
    // The RESOURCE a grant is taken on is the function's own ARN as Lambda reports it, so
    // it is resolved there rather than assumed to be what the request carried — and the
    // read is also the existence question: a function this account does not hold is one no
    // invocation could ever reach.
    let resolved = match client
        .get_function_configuration()
        .function_name(lambda_function_arn)
        .send()
        .await
    {
        Ok(reported) => reported
            .function_arn
            .unwrap_or_else(|| lambda_function_arn.to_string()),
        Err(error)
            if error.as_service_error().is_some_and(
                ::aws_sdk_lambda::operation::get_function_configuration::GetFunctionConfigurationError::is_resource_not_found_exception,
            ) =>
        {
            return FunctionValidation::Denied;
        }
        Err(error) => {
            ::tracing::warn!(
                function = %lambda_function_arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "nothing answered about a certificate provider's Lambda function; the \
                 provider is created without that check"
            );
            return FunctionValidation::Unanswered;
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
                "nothing answered about the resource policy of a certificate provider's \
                 Lambda function; the provider is created without that check"
            );
            return FunctionValidation::Unanswered;
        }
    };
    // The decision is `service_common::iam_authorization`, the tree's one evaluation of
    // policy documents, so this service and every other one handed the same document
    // decide alike. The keys the invocation would carry are stated, so a grant narrowed to
    // one provider admits that provider's invocations and no other's.
    let principals = [super::SERVICE_PRINCIPAL.to_string()];
    let context = [
        (SOURCE_ARN_KEY.to_string(), provider_arn.to_string()),
        (
            SOURCE_ACCOUNT_KEY.to_string(),
            ::engine::emulator_account_id().to_string(),
        ),
    ];
    let request = Request::new(INVOKE_FUNCTION_ACTION, &resolved, &principals)
        .with_context(&context)
        .with_published_keys(&[SOURCE_ARN_KEY, SOURCE_ACCOUNT_KEY]);
    if resource_decision(&document, &request) == Decision::Allowed {
        FunctionValidation::Admitted
    } else {
        FunctionValidation::Denied
    }
}
