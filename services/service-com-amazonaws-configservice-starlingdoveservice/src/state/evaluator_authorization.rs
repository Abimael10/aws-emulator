// SPDX-License-Identifier: CC0-1.0

//! Whether Config MAY invoke the AWS Lambda function a custom rule names as its evaluator.
//!
//! A Config Custom Lambda rule does not evaluate itself: Config invokes the function the
//! account points the rule at ([`super::custom_lambda_rule`]). So a rule whose evaluator
//! Config cannot invoke is a rule nothing would ever evaluate, and `PutConfigRule` models
//! the refusal of exactly that — `InsufficientPermissionsException`, whose own
//! documentation states the case: "For PutConfigRule, the Lambda function cannot be
//! invoked. Check the function ARN, and check the function's permissions."
//!
//! Those are the two readings this module makes, in that order. The ARN has to name a
//! function this account holds, which is AWS Lambda's own answer about it; and because
//! Config invokes as its own service principal, the permission has to come from THAT
//! FUNCTION'S resource-based policy — the document `AddPermission` writes — rather than
//! from the caller's own.
//!
//! Both are read from AWS Lambda ITSELF over the in-process callback, through the one
//! client this crate reaches Lambda with ([`super::custom_lambda_rule::lambda_client`]), so
//! this crate names no other service crate; and the decision the policy gives is the
//! workspace's ONE evaluation of an IAM policy document
//! (`service_common::iam_authorization`) rather than a second reading of that grammar here,
//! exactly as the delivery channel's bucket policy is read
//! ([`super::delivery_authorization`]).
//!
//! A read that establishes nothing grounds no refusal ([`EvaluatorAuthorization::Unanswered`]):
//! an emulator with no AWS Lambda to ask refuses no rule, which is the same direction the
//! delivery reading takes for a bucket S3 answered nothing about.
//!
//! The condition context is the one every call Config makes as its service principal
//! carries: the account the rule belongs to and the ARN of the rule the invocation is made
//! for — the two keys a grant to a service principal is scoped by, and the two the
//! documented `AddPermission` for Config constrains.

use ::service_common::iam_authorization::{resource_decision, Decision, Request};

use super::custom_lambda_rule::lambda_client;
use super::delivery_authorization::{CONFIG_SERVICE_PRINCIPAL, SOURCE_ACCOUNT_KEY, SOURCE_ARN_KEY};

/// The IAM action invoking a rule's evaluator is, which is what the function's
/// resource-based policy has to allow the Config service principal.
const INVOKE_FUNCTION_ACTION: &str = "lambda:InvokeFunction";

/// The code AWS Lambda answers for a function this account does not hold, and for a
/// function that carries no resource-based policy at all — the two readings that each leave
/// Config with nothing it could invoke.
const LAMBDA_RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// How Config words `InsufficientPermissionsException` for a custom rule whose evaluator it
/// cannot invoke: the service states the case the exception documents and NAMES the function
/// it was about, which is what tells a client which function's ARN and policy to check.
///
/// The fixed text is the exception's own statement of this case (`smithy_query(query="doc",
/// shape="InsufficientPermissionsException")`: "the Lambda function cannot be invoked. Check
/// the function ARN, and check the function's permissions"), and the one variable part is
/// the function the request named — the rule's `SourceIdentifier`, "the Amazon Resource Name
/// (ARN) that Lambda assigns to the function" — quoted as the request spelled it. It names
/// the function rather than the rule, because the function is what could not be invoked.
pub fn cannot_invoke_evaluator_message(function: &str) -> String {
    format!(
        "The AWS Lambda function {function} cannot be invoked. Check the function ARN, and \
         check the function's permissions."
    )
}

/// What the function behind a custom rule's evaluator says about Config invoking it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluatorAuthorization {
    /// The ARN names a function of this account and that function's own policy allows the
    /// Config service principal to invoke it.
    Permitted,
    /// Config cannot invoke it: the ARN names no function of this account, or the function
    /// carries no policy admitting Config — which is `InsufficientPermissionsException`.
    Denied,
    /// AWS Lambda established nothing: it answered neither about the function nor that it
    /// holds none, so there is nothing for this decision to read.
    Unanswered,
}

/// What the function `function` says about Config invoking it as the evaluator of a rule,
/// read from AWS Lambda over the in-process callback.
///
/// `function` is the rule's `SourceIdentifier` — "the Amazon Resource Name (ARN) that
/// Lambda assigns to the function" — and `config_rule_arn` is the ARN of the rule the
/// invocation would be made for, where the rule already has one, for a policy that scopes
/// its grant to it.
pub async fn evaluator_authorization(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    function: &str,
    config_rule_arn: Option<&str>,
) -> EvaluatorAuthorization {
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

    let client = lambda_client(callback, region);
    // "Check the function ARN": a value naming no function of this account leaves Config
    // nothing to invoke, and the ARN Lambda reports for it is the resource the function's
    // own policy grants on.
    let function_arn = match client
        .get_function_configuration()
        .function_name(function)
        .send()
        .await
    {
        Ok(answer) => match answer.function_arn {
            Some(arn) => arn,
            // Lambda answered without naming the function it answered about, so nothing
            // about the grant has been established.
            None => return EvaluatorAuthorization::Unanswered,
        },
        Err(error) => {
            return match error.code() {
                Some(LAMBDA_RESOURCE_NOT_FOUND) => EvaluatorAuthorization::Denied,
                _ => EvaluatorAuthorization::Unanswered,
            }
        }
    };
    // "check the function's permissions": Config invokes as its own service principal, so
    // the grant has to come from the function's own resource-based policy.
    let document = match client.get_policy().function_name(function).send().await {
        Ok(answer) => answer.policy.unwrap_or_default(),
        Err(error) => {
            return match error.code() {
                // A function carrying no policy at all allows Config nothing.
                Some(LAMBDA_RESOURCE_NOT_FOUND) => EvaluatorAuthorization::Denied,
                _ => EvaluatorAuthorization::Unanswered,
            }
        }
    };
    if invocation_is_allowed(&document, &function_arn, config_rule_arn) {
        EvaluatorAuthorization::Permitted
    } else {
        EvaluatorAuthorization::Denied
    }
}

/// Whether the resource-based policy `document` in force on the function `function_arn`
/// allows the Config service principal to invoke it for the rule `config_rule_arn`.
///
/// Decided by the shared IAM evaluation, in the condition context a call Config makes as its
/// service principal carries.
pub fn invocation_is_allowed(
    document: &str,
    function_arn: &str,
    config_rule_arn: Option<&str>,
) -> bool {
    let account_id = ::emulator_server::emulator_account_id().to_string();
    let principals = [CONFIG_SERVICE_PRINCIPAL.to_string()];
    let mut context = vec![(SOURCE_ACCOUNT_KEY.to_string(), account_id)];
    if let Some(source_arn) = config_rule_arn {
        context.push((SOURCE_ARN_KEY.to_string(), source_arn.to_string()));
    }
    let request =
        Request::new(INVOKE_FUNCTION_ACTION, function_arn, &principals).with_context(&context);
    resource_decision(document, &request) == Decision::Allowed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The function every case below is about, and the rule the invocation would be made
    /// for.
    const FUNCTION_ARN: &str = "arn:aws:lambda:us-west-2:000000000000:function:evaluator";
    /// See [`FUNCTION_ARN`].
    const RULE_ARN: &str = "arn:aws:config:us-west-2:000000000000:config-rule/config-rule-abc123";

    /// One statement of a function's resource-based policy, as `AddPermission` writes one:
    /// the principal it grants to, the action, the resource and any condition.
    fn policy(principal: &str, action: &str, resource: &str, condition: ::serde_json::Value) -> String {
        let mut statement = ::serde_json::json!({
            "Sid": "AllowExecutionFromConfig",
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": action,
            "Resource": resource,
        });
        if !condition.is_null() {
            statement["Condition"] = condition;
        }
        ::serde_json::json!({ "Version": "2012-10-17", "Statement": [statement] }).to_string()
    }

    /// A function's policy admits Config's invocation exactly when a statement grants the
    /// Config service principal `lambda:InvokeFunction` on that function: the rows differ
    /// along the principal granted to, the action granted, the resource granted on and the
    /// condition the grant is scoped by (none, the account, the rule's own ARN, another
    /// account's).
    #[test]
    fn a_functions_policy_decides_whether_config_may_invoke_it() {
        let account = ::emulator_server::emulator_account_id();
        let cases: [(&str, String, bool); 8] = [
            (
                "the documented grant, unconditioned",
                policy(
                    CONFIG_SERVICE_PRINCIPAL,
                    INVOKE_FUNCTION_ACTION,
                    FUNCTION_ARN,
                    ::serde_json::Value::Null,
                ),
                true,
            ),
            (
                "the documented grant, scoped to this account",
                policy(
                    CONFIG_SERVICE_PRINCIPAL,
                    INVOKE_FUNCTION_ACTION,
                    FUNCTION_ARN,
                    ::serde_json::json!({
                        "StringEquals": { "AWS:SourceAccount": account }
                    }),
                ),
                true,
            ),
            (
                "the documented grant, scoped to the rule it is made for",
                policy(
                    CONFIG_SERVICE_PRINCIPAL,
                    INVOKE_FUNCTION_ACTION,
                    FUNCTION_ARN,
                    ::serde_json::json!({ "ArnLike": { "AWS:SourceArn": RULE_ARN } }),
                ),
                true,
            ),
            (
                "a grant of every Lambda action",
                policy(
                    CONFIG_SERVICE_PRINCIPAL,
                    "lambda:*",
                    FUNCTION_ARN,
                    ::serde_json::Value::Null,
                ),
                true,
            ),
            (
                "a grant to another service principal",
                policy(
                    "events.amazonaws.com",
                    INVOKE_FUNCTION_ACTION,
                    FUNCTION_ARN,
                    ::serde_json::Value::Null,
                ),
                false,
            ),
            (
                "a grant of another action",
                policy(
                    CONFIG_SERVICE_PRINCIPAL,
                    "lambda:GetFunction",
                    FUNCTION_ARN,
                    ::serde_json::Value::Null,
                ),
                false,
            ),
            (
                "a grant on another function",
                policy(
                    CONFIG_SERVICE_PRINCIPAL,
                    INVOKE_FUNCTION_ACTION,
                    "arn:aws:lambda:us-west-2:000000000000:function:other",
                    ::serde_json::Value::Null,
                ),
                false,
            ),
            (
                "a grant scoped to another account",
                policy(
                    CONFIG_SERVICE_PRINCIPAL,
                    INVOKE_FUNCTION_ACTION,
                    FUNCTION_ARN,
                    ::serde_json::json!({
                        "StringEquals": { "AWS:SourceAccount": "210987654321" }
                    }),
                ),
                false,
            ),
        ];
        for (case, document, expected) in cases {
            let allowed = invocation_is_allowed(&document, FUNCTION_ARN, Some(RULE_ARN));
            assert_eq!(
                allowed, expected,
                "case {case:?}: expected the invocation to be allowed={expected}, got \
                 {allowed} for {document}"
            );
        }
    }

    /// A function carrying no policy at all, and a document that is not a policy, allow
    /// Config nothing — the default deny, rather than a grant read out of text that states
    /// none.
    #[test]
    fn a_function_with_no_readable_policy_allows_config_nothing() {
        for (case, document) in [("no policy at all", ""), ("not a policy", "{}")] {
            let allowed = invocation_is_allowed(document, FUNCTION_ARN, Some(RULE_ARN));
            assert!(
                !allowed,
                "case {case:?}: expected the invocation to be denied, got allowed={allowed}"
            );
        }
    }

    /// The refusal states Config's fixed sentence around the one function the request named,
    /// whatever form that identifier takes: the rows differ along the identifier.
    #[test]
    fn the_refusal_names_the_function_it_could_not_invoke() {
        for (case, function) in [
            ("a function ARN", FUNCTION_ARN),
            (
                "an ARN of another Region",
                "arn:aws:lambda:eu-west-1:000000000000:function:evaluator",
            ),
            ("a bare function name", "evaluator"),
        ] {
            let message = cannot_invoke_evaluator_message(function);
            assert_eq!(
                message,
                format!(
                    "The AWS Lambda function {function} cannot be invoked. Check the function \
                     ARN, and check the function's permissions."
                ),
                "case {case:?}: unexpected wording {message:?}"
            );
        }
    }
}
