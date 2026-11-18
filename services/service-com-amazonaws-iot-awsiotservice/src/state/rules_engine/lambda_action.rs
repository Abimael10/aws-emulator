// SPDX-License-Identifier: CC0-1.0

//! `Action$lambda`, "Invoke a Lambda function": the one hop the rules engine makes into AWS
//! Lambda, and the grant that decides whether it is made at all.
//!
//! A `lambda` action names only a function (`LambdaAction$functionArn` is its single
//! `@required` member — there is no `roleArn`), because AWS IoT invokes it AS ITS OWN SERVICE
//! PRINCIPAL ([`super::SERVICE_PRINCIPAL`]) rather than under a role the rule carries. What
//! admits the invocation is therefore the FUNCTION's resource-based policy — the document
//! `AddPermission` writes and `GetPolicy` reports — evaluated on
//! [`INVOKE_FUNCTION_ACTION`] over that function with the RULE's own ARN as the
//! `aws:SourceArn` such a grant is narrowed by ([`admitted`]). So a permission added for one
//! rule admits that rule's invocations and no other's, a permission naming another service
//! principal admits none, and a function carrying no policy at all admits none — which is the
//! only observable the IoT -> Lambda authorization boundary has, since the publisher is
//! answered the same either way.
//!
//! The policy is read FROM LAMBDA ITSELF, not held a second time here, so a permission added
//! or removed there takes effect on the next matching message; and the decision is
//! [`::service_common::iam_authorization`], the tree's one evaluation of policy documents, so
//! this service and every other one handed the same document decide alike.
//!
//! The invocation is ASYNCHRONOUS — "Event – Invoke the function asynchronously … The API
//! response only includes a status code" (`Invoke$InvocationType`) — which is how AWS IoT
//! invokes a rule's function: the rules engine hands the message over and does not wait for
//! the handler, so what the HANDLER then did (including raising) is no part of the action's
//! outcome. What IS the action's outcome is whether the invocation was accepted, and that is
//! what the rule's `errorAction` is run for ([`super::dispatch_published_message`]).
//!
//! Every call goes over the emulator's own IN-PROCESS transport — this crate's one construction
//! of the Lambda client ([`super::super::custom_authorizer::lambda_client`], which
//! `TestInvokeAuthorizer` reaches a function through too) — so the function a rule names reaches
//! only what this emulator serves itself: no socket is opened and no host name is resolved.

use ::std::time::Duration;

use super::super::custom_authorizer::{lambda_client, LAMBDA_ARN_NAMESPACE};
use super::refusal_of;
use crate::ops::types::LambdaAction;

/// The IAM action an invocation of a function performs, which is not the operation's own name:
/// `Invoke` "requires permission for the `lambda:InvokeFunction` action".
pub const INVOKE_FUNCTION_ACTION: &str = "lambda:InvokeFunction";

/// The condition key a grant narrowed to ONE invoking resource names the source by, which for
/// a topic rule's action is the RULE the message matched.
const SOURCE_ARN_KEY: &str = "aws:SourceArn";

/// The condition key a grant narrowed to the account that owns the invoking resource names it
/// by.
const SOURCE_ACCOUNT_KEY: &str = "aws:SourceAccount";

/// How long one invocation may take to be ACCEPTED before this engine stops waiting on it.
///
/// An asynchronous invocation is answered as soon as Lambda has taken the event, so this is a
/// ceiling on that acknowledgement rather than on the handler's run — generous, because the
/// bound exists only so that one unanswered call cannot hold the publish that matched the rule
/// open for ever.
const ACCEPTANCE_CEILING: Duration = Duration::from_secs(60);

/// What a call that was never answered inside [`ACCEPTANCE_CEILING`] reports.
const NOT_ANSWERED: &str = "the function did not answer the invocation";

/// Invoke the function a `lambda` action names with `message` as the invocation event, for the
/// rule whose ARN is `source_arn`.
///
/// `Err` is why the action was NOT carried out, worded as the refusal that withheld it: the
/// target service's own message where Lambda answered one ([`super::refusal_of`], the one place
/// this engine words a target's refusal), and the sentence AWS words for an action no
/// resource-based policy allows where the function's policy carries no matching grant. That is
/// what the rule's `errorAction` reports, so it is the one value this call hands back.
pub async fn invoke(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    source_arn: &str,
    action: &LambdaAction,
    message: &[u8],
) -> Result<(), String> {
    // The function is reached through the Region its own ARN names, so a rule naming a function
    // in another Region invokes it there; an ARN naming no Region label is addressed to the
    // Region the publish was signed for.
    let region = ::service_common::arn::region_of(&action.function_arn, LAMBDA_ARN_NAMESPACE)
        .unwrap_or(request_region);
    let client = lambda_client(ctx, region);
    admitted(&client, &action.function_arn, source_arn).await?;
    let invocation = client
        .invoke()
        .function_name(&action.function_arn)
        .invocation_type(::aws_sdk_lambda::types::InvocationType::Event)
        .payload(::aws_sdk_lambda::primitives::Blob::new(message.to_vec()))
        .send();
    let Ok(answered) = ::tokio::time::timeout(ACCEPTANCE_CEILING, invocation).await else {
        return Err(NOT_ANSWERED.to_string());
    };
    match answered {
        Ok(_) => Ok(()),
        // Lambda refused the invocation, so nothing ran: the action failed, and the refusal
        // Lambda itself worded is what says why.
        Err(error) => Err(refusal_of(&error)),
    }
}

/// Whether the resource policy of the function `function_arn` admits the invocation the rule
/// whose ARN is `source_arn` makes.
///
/// A function whose policy Lambda REPORTS as absent carries none ([`reports_no_policy`]), which
/// grants nothing, so the invocation is not made — that is the case a rule naming a function
/// nobody permitted is in. A function Lambda answers NOTHING about — one the account does not
/// hold, which is what a rule naming the wrong ARN names — is that service's own refusal, and so
/// is any other answer it gives about the function's policy: there is no reading of either under
/// which the grant was shown, and admitting an invocation no grant was read for would make the
/// permission boundary stop being observable. Each is reported as the failure it is, worded by
/// whatever refused it.
async fn admitted(
    client: &::aws_sdk_lambda::Client,
    function_arn: &str,
    source_arn: &str,
) -> Result<(), String> {
    // The RESOURCE a grant is taken on is the function's own ARN as Lambda reports it, so it is
    // resolved there rather than assumed to be what the action carried.
    let resolved = client
        .get_function_configuration()
        .function_name(function_arn)
        .send()
        .await
        .map_err(|error| refusal_of(&error))?
        .function_arn
        .unwrap_or_else(|| function_arn.to_string());
    let document = match client.get_policy().function_name(&resolved).send().await {
        Ok(reported) => reported.policy.unwrap_or_default(),
        // A function Lambda reports no resource policy for carries none, which grants nothing.
        Err(error) if reports_no_policy(&error) => String::new(),
        Err(error) => return Err(refusal_of(&error)),
    };
    let principals = [super::SERVICE_PRINCIPAL.to_string()];
    let context = [
        (SOURCE_ARN_KEY.to_string(), source_arn.to_string()),
        (
            SOURCE_ACCOUNT_KEY.to_string(),
            ::engine::emulator_account_id().to_string(),
        ),
    ];
    let request = ::service_common::iam_authorization::Request::new(
        INVOKE_FUNCTION_ACTION,
        &resolved,
        &principals,
    )
    .with_context(&context)
    .with_published_keys(&[SOURCE_ARN_KEY, SOURCE_ACCOUNT_KEY]);
    if ::service_common::iam_authorization::resource_decision(&document, &request)
        == ::service_common::iam_authorization::Decision::Allowed
    {
        return Ok(());
    }
    Err(
        ::service_common::iam_authorization::not_authorized_by_resource_policy(
            super::SERVICE_PRINCIPAL,
            INVOKE_FUNCTION_ACTION,
            &resolved,
        ),
    )
}

/// Whether `error` is LAMBDA REPORTING THAT THE FUNCTION HAS NO RESOURCE POLICY, which is an
/// answer about the function — `GetPolicy` models `ResourceNotFoundException` for "the resource
/// specified in the request does not exist", and a function carrying no policy grants nothing.
///
/// Every other failure is not that answer, so it shows no grant either: it is reported as the
/// refusal it is.
fn reports_no_policy(
    error: &::aws_smithy_runtime_api::client::result::SdkError<
        ::aws_sdk_lambda::operation::get_policy::GetPolicyError,
        ::aws_smithy_runtime_api::client::orchestrator::HttpResponse,
    >,
) -> bool {
    let ::aws_smithy_runtime_api::client::result::SdkError::ServiceError(service) = error else {
        return false;
    };
    service.err().is_resource_not_found_exception()
}
