// SPDX-License-Identifier: CC0-1.0

//! Whether the LAMBDA FUNCTION a certificate provider names is one this service could
//! call to sign a certificate.
//!
//! A certificate provider IS a Lambda function AWS IoT invokes instead of signing a
//! certificate itself: "all API calls to CreateCertificateFromCsr will invoke the
//! certificate provider to create the certificates" (`CreateCertificateProvider`). The
//! invocation is made BY THIS SERVICE PRINCIPAL rather than under a role the provider
//! carries, so what admits it is the FUNCTION's own resource-based policy, read with the
//! PROVIDER's own ARN as the `aws:SourceArn` such a grant is narrowed by. That reading is
//! the one this service takes of every function it invokes — the function a topic rule
//! names and the function a custom authorizer authorizes with alike — so it lives once, in
//! [`super::lambda_invoke_grant`], and this module only says which resource the invocation
//! would be made for and how the refusal of a provider is worded.
//!
//! The function is resolved BEFORE the provider record is written, because a provider
//! reported ready over a function that cannot be called is a provider whose certificate
//! issuance fails later with no diagnosable cause.
//!
//! A read that establishes NOTHING (no AWS Lambda is mounted to ask, or the call could not
//! be dispatched) refuses nothing: this is where this reading PARTS from the rules engine's
//! — a rule action that cannot read a grant must not be performed, while a create must not
//! be refused on evidence this emulator does not have.

use super::lambda_invoke_grant::invocation_grant;

/// What resolving a certificate provider's Lambda function established — the three
/// outcomes every reading of a function's grant has
/// ([`super::lambda_invoke_grant::InvocationGrant`]), under the name this plane's handlers
/// already match on.
pub use super::lambda_invoke_grant::InvocationGrant as FunctionValidation;

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
/// Call it with this service's state lock RELEASED, for the reason
/// [`invocation_grant`] documents.
pub async fn validate_provider_function(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    provider_arn: &str,
    lambda_function_arn: &str,
) -> FunctionValidation {
    invocation_grant(ctx, request_region, lambda_function_arn, provider_arn).await
}
