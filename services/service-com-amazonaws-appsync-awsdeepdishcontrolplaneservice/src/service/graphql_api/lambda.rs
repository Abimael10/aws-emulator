// SPDX-License-Identifier: CC0-1.0

//! The ONE in-process hop a GraphQL API's data plane makes to AWS Lambda: how the function an
//! `AWS_LAMBDA` data source, or an `AWS_LAMBDA` authorization mode, names is RUN, and how what
//! came back is told from the call never reaching a function at all.
//!
//! Two of an API's steps are one synchronous invocation of a function this emulator holds:
//!
//! * a resolver (or one function of a pipeline) bound to a data source whose type is
//!   `AWS_LAMBDA` — "the data source is an Lambda function" (`DataSource$type`) — whose
//!   `lambdaConfig` carries "the Amazon Resource Name (ARN) for the Lambda function"
//!   (`LambdaDataSourceConfig$lambdaFunctionArn`);
//! * the authorizer of an API whose authorization mode is `AWS_LAMBDA`, whose
//!   `lambdaAuthorizerConfig` carries "the Amazon Resource Name (ARN) of the Lambda function to
//!   be called for authorization" (`LambdaAuthorizerConfig$authorizerUri`).
//!
//! The invocation is made THROUGH THE EMULATOR this service runs within, over the in-process
//! transport the data plane was handed when the API was created
//! (`PlaneContext::callback`): nothing is invented and no connection is opened, so what a
//! field resolves to is what the function itself returned for the event it was given.
//!
//! Three outcomes are told apart, because a field reports each differently:
//!
//! * the function RAN — its payload, plus the `FunctionError` Lambda reports for a handler
//!   that raised ("X-Amz-Function-Error … indicates whether an error occurred while executing
//!   the Lambda function"), which is what says the payload is an error document rather than a
//!   result;
//! * the function could not be invoked — Lambda itself refused the invocation (no such
//!   function, a function that cannot be invoked yet);
//! * the call reached NO Lambda at all, because this emulator mounts none
//!   ([`::service_common::in_process_refusal`]).
//!
//! WHO the invocation is made as is not the API's caller: AppSync invokes a data source's
//! function under the data source's own service role — "the system assumes this role when
//! accessing the data source" (`DataSource$serviceRoleArn`) — so what admits the invocation is
//! what IAM holds in force on THAT role ([`role_refusal`]), read from IAM itself so a policy
//! written there takes effect on the next request.

use ::std::time::Duration;

use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;
use ::serde_json::Value as Json;
use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_common::iam_authorization::{self as iam, Decision};
use ::smithy_server_runtime::EmbeddedEmulator;

/// The `aws.api#service$arnNamespace` a function ARN carries, which is what a value naming a
/// function is matched against.
const LAMBDA_ARN_VENDOR: &str = "lambda";

/// A function ARN carries no `/`, so a wrapper's own path segments end it.
const ARN_PATH_SEPARATOR: char = '/';

/// The IAM action an invocation of a function performs, which is not the operation's own name:
/// `Invoke` "requires permission for the `lambda:InvokeFunction` action".
pub const INVOKE_FUNCTION_ACTION: &str = "lambda:InvokeFunction";

/// The session name AppSync assumes a data source's service role under, which is what the
/// principal of an invocation IS: "the system assumes this role when accessing the data source"
/// (`DataSource$serviceRoleArn`), so what a refusal of that invocation blames is the SESSION of
/// that role rather than the role itself.
const ASSUMED_SESSION_NAME: &str = "APPSYNC_ASSUME_ROLE";

/// How long one invocation of a data source's or an authorizer's function is given to answer.
///
/// An emulator resource guard, not an AWS quota: the request that is waiting on it is served on
/// one of this endpoint's own connections, so a function that never answers may not hold that
/// connection for the whole request deadline. Generous on purpose — what a case asserts is what
/// the function returned, never how fast the host ran it.
pub const INVOCATION_DEADLINE: Duration = Duration::from_secs(30);

/// The members Lambda's own error document carries for a handler that RAISED: "the error type"
/// the runtime reports — which for an unhandled exception is the exception's class name — and
/// the exception's own text.
const ERROR_TYPE_MEMBER: &str = "errorType";
const ERROR_MESSAGE_MEMBER: &str = "errorMessage";

/// The function one hop addresses: the ARN to invoke, and the Region whose Lambda endpoint the
/// call is made to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionTarget {
    /// The function ARN, qualified exactly as the API's record names it — a version or alias
    /// suffix included, since `Invoke` resolves the qualifier the ARN carries.
    pub arn: String,
    /// The Region of that ARN, which is the endpoint the invocation is addressed to.
    pub region: String,
}

/// The function `value` names, or `None` when it names none.
///
/// Both spellings an AppSync record carries resolve, because both reach this hop: a data
/// source's `lambdaFunctionArn` IS the function ARN, and an authorizer's `authorizerUri` is "a
/// standard Lambda ARN, a version ARN (`.../v3`), or an alias ARN". The ARN is found by its OWN
/// fields rather than by stripping a wrapper — the first `arn:` whose service field is `lambda`
/// — so a value that embeds one in a path resolves the same function.
///
/// The Region is read through [`::service_common::arn::region_of`], the tree's one owner of
/// that rule, which reports the field only when it is a Region LABEL: the value ends up in the
/// authority of the endpoint the invocation is addressed to, so a field carrying anything else
/// names no endpoint and the value addresses nothing.
#[must_use]
pub fn function_target(value: &str) -> Option<FunctionTarget> {
    let candidate = value
        .match_indices("arn:")
        .map(|(start, _)| &value[start..])
        .find_map(|rest| {
            let arn = rest.split(ARN_PATH_SEPARATOR).next()?;
            (::service_common::arn::vendor(arn) == Some(LAMBDA_ARN_VENDOR)).then_some(arn)
        })?;
    let region = ::service_common::arn::region_of(candidate, LAMBDA_ARN_VENDOR)?;
    Some(FunctionTarget {
        arn: candidate.to_string(),
        region: region.to_string(),
    })
}

/// What the function RETURNED: the payload bytes, and the error signal Lambda sets for a
/// handler that raised rather than returned.
#[derive(Debug, Clone)]
pub struct Ran {
    /// The function's own response bytes — `Invoke`'s `Payload`, which for a handler that
    /// raised is Lambda's error document.
    pub payload: Vec<u8>,
    /// `FunctionError`: "If present, indicates that an error occurred during function
    /// execution. Details about the error are included in the response payload."
    pub function_error: Option<String>,
}

impl Ran {
    /// Whether the run ENDED IN AN ERROR the handler did not handle, which is what makes its
    /// payload an error document rather than a value.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.function_error
            .as_deref()
            .is_some_and(|signal| !signal.trim().is_empty())
    }

    /// The function's answer as a value, or `Json::Null` for a payload that is not a JSON
    /// document at all — a handler that returned nothing returns nothing.
    #[must_use]
    pub fn value(&self) -> Json {
        ::serde_json::from_slice(&self.payload).unwrap_or(Json::Null)
    }

    /// The CLASS and the SENTENCE of the error a failed run reported, as Lambda's own error
    /// document states them: `errorType`, which for an unhandled exception is the raised
    /// exception's class name, and `errorMessage`, which is the exception's text.
    ///
    /// A payload carrying neither is reported under the signal Lambda set for the run, which is
    /// the only thing then known about it.
    #[must_use]
    pub fn raised(&self) -> (String, String) {
        let document = self.value();
        let member = |name: &str| {
            document
                .get(name)
                .and_then(Json::as_str)
                .map(str::to_string)
                .filter(|text| !text.is_empty())
        };
        let class = member(ERROR_TYPE_MEMBER).unwrap_or_else(|| {
            self.function_error
                .clone()
                .unwrap_or_else(|| UNHANDLED_ERROR_TYPE.to_string())
        });
        let message = member(ERROR_MESSAGE_MEMBER).unwrap_or_else(|| class.clone());
        (class, message)
    }
}

/// The `errorType` a run that reported no class of its own is reported under: Lambda's own
/// signal for a handler that raised and named nothing.
const UNHANDLED_ERROR_TYPE: &str = "Unhandled";

/// Why no function answer exists.
#[derive(Debug, Clone)]
pub enum Unreached {
    /// Lambda ANSWERED, refusing the invocation (no such function, a function that cannot be
    /// invoked yet, an invocation it cannot perform). The reason is Lambda's own.
    Refused(String),
    /// Nothing in this emulator serves Lambda, so the invocation reached no function.
    NoLambda,
    /// The function was invoked and had not answered when the deadline elapsed.
    TimedOut(Duration),
}

impl Unreached {
    /// One sentence about the invocation that produced no answer, which is what the field it
    /// was made for reports as the reason it did not resolve.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Refused(detail) => detail.clone(),
            Self::NoLambda => NO_LAMBDA.to_string(),
            Self::TimedOut(after) => format!(
                "The function did not answer within {} seconds.",
                after.as_secs()
            ),
        }
    }
}

/// The sentence an invocation that reached no AWS Lambda at all states: this emulator mounts
/// none, so there is no function answer to resolve the field from.
const NO_LAMBDA: &str = "No AWS Lambda is mounted in this emulator, so the data source's \
                         function was not invoked.";

/// Run `target` with `event` as its event document and answer what it returned.
///
/// The invocation is SYNCHRONOUS — "RequestResponse (default) – Invoke the function
/// synchronously. Keep the connection open until the function returns a response or times out"
/// — because the field's value IS the function's answer, so there is nothing to resolve until
/// the run ends.
pub async fn invoke(
    callback: &EmbeddedEmulator,
    target: &FunctionTarget,
    event: &Json,
) -> Result<Ran, Unreached> {
    let payload = ::serde_json::to_vec(event).unwrap_or_default();
    let call = client(callback, &target.region)
        .invoke()
        .function_name(&target.arn)
        .payload(::aws_smithy_types::Blob::new(payload))
        .send();
    let answered = match ::tokio::time::timeout(INVOCATION_DEADLINE, call).await {
        Ok(answered) => answered,
        Err(_elapsed) => return Err(Unreached::TimedOut(INVOCATION_DEADLINE)),
    };
    let output = match answered {
        Ok(output) => output,
        Err(error) => {
            // The bridge keeps the refused call's BODY, which is where the front door's own
            // refusal can be told from a refusal Lambda itself answered.
            let raw = error
                .raw_response()
                .and_then(|response| response.body().bytes())
                .map(<[u8]>::to_vec)
                .unwrap_or_default();
            if ::service_common::in_process_refusal::is_transport_refusal(&raw) {
                return Err(Unreached::NoLambda);
            }
            return Err(Unreached::Refused(
                error
                    .message()
                    .map(str::to_string)
                    .unwrap_or_else(|| error.code().unwrap_or("Unknown").to_string()),
            ));
        }
    };
    Ok(Ran {
        payload: output
            .payload
            .map(::aws_smithy_types::Blob::into_inner)
            .unwrap_or_default(),
        function_error: output.function_error,
    })
}

/// Why the role `role_arn` may NOT invoke `function_arn`, or `None` when nothing withholds the
/// invocation from it.
///
/// AppSync invokes a data source's function under the data source's own service role, so the
/// decision is the IDENTITY-based one taken on that role: the documents IAM holds in force on
/// it, judged on [`INVOKE_FUNCTION_ACTION`] over the function, through
/// [`::service_common::iam_authorization`] — the tree's one evaluation of policy documents, so
/// a policy written about a function here is read exactly as a policy about any other call is.
/// A permissions boundary CAPS what those documents grant, as it does everywhere in IAM.
///
/// `None` also when nothing about the role could be SHOWN, which withholds nothing: a data
/// source naming no service role, an emulator with no IAM mounted, a role this account's IAM
/// does not hold (it may belong to another account) and a role holding an attachment whose
/// document this emulator does not carry are not evidence of a missing grant — the invocation
/// is then what decides the field.
#[must_use]
pub fn role_refusal(
    engine: &::engine::InMemoryEngine,
    role_arn: Option<&str>,
    function_arn: &str,
) -> Option<String> {
    let role_arn = role_arn.filter(|arn| !arn.is_empty())?;
    let iam_state = iam_store::service_state(engine)?;
    let store = iam_state.lock();
    // A value this account's IAM holds no role under says nothing about what that role may do.
    store.roles.values().find(|role| role.arn == role_arn)?;
    let policies = store.principal_policies(role_arn)?;
    if !policies.uncarried.is_empty() {
        return None;
    }
    let principals = [role_arn.to_string()];
    let request = iam::Request::new(INVOKE_FUNCTION_ACTION, function_arn, &principals);
    let said = iam::identity_decision(&policies.documents, &request);
    // WHO the invocation is made as: the session of that role, which is the principal a refusal
    // of it names.
    let principal = session_principal(role_arn);
    if said == Decision::Denied {
        return Some(iam::not_authorized_by_identity_policy(
            &principal,
            INVOKE_FUNCTION_ACTION,
            function_arn,
            iam::IDENTITY_POLICY_EXPLICIT_DENY,
        ));
    }
    if said != Decision::Allowed {
        return Some(iam::not_authorized_by_identity_policy(
            &principal,
            INVOKE_FUNCTION_ACTION,
            function_arn,
            &iam::no_identity_policy_allows(INVOKE_FUNCTION_ACTION),
        ));
    }
    // "A permissions boundary sets the maximum permissions that an identity-based policy can
    // grant": a grant the boundary does not also allow is not a permission.
    let boundary = store.permissions_boundary_document(role_arn)?;
    (iam::identity_decision(::std::slice::from_ref(&boundary), &request) != Decision::Allowed)
        .then(|| {
            iam::not_authorized_by_identity_policy(
                &principal,
                INVOKE_FUNCTION_ACTION,
                function_arn,
                &iam::no_permissions_boundary_allows(INVOKE_FUNCTION_ACTION),
            )
        })
}

/// The principal a SESSION of `role_arn` is: the partition and the account are the role's own,
/// read off its ARN through the one ARN parse this workspace has, so a role of another account
/// is named as that role's session rather than as this emulator's.
fn session_principal(role_arn: &str) -> String {
    let parsed = ::service_common::arn::parse(role_arn);
    let partition = parsed.as_ref().map_or("aws", |arn| arn.partition);
    let account = parsed.as_ref().map_or("", |arn| arn.account);
    let role_name = ::service_common::iam_role::role_name(role_arn).unwrap_or(role_arn);
    format!("arn:{partition}:sts::{account}:assumed-role/{role_name}/{ASSUMED_SESSION_NAME}")
}

/// The Lambda client one hop is made with.
///
/// The transport is the emulator's own, in process, so the invocation is served by the Lambda
/// this emulator mounts and no connection is opened; the credentials are the ones the callback
/// hands over, which sign this call and are never reported or logged. The Region only selects
/// the endpoint the client is configured for — the transport serves it in process whatever that
/// label says.
fn client(callback: &EmbeddedEmulator, region: &str) -> ::aws_sdk_lambda::Client {
    let config = ::aws_sdk_lambda::Config::builder()
        .behavior_version(::aws_sdk_lambda::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_lambda::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://lambda.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client is
        // not given. [`INVOCATION_DEADLINE`] is the deadline, applied by the caller above.
        .retry_config(::aws_sdk_lambda::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_lambda::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_lambda::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_lambda::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_lambda::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row: what a failing assertion calls it, the value read, and the `(ARN, Region)` the
    /// function it names resolves to.
    type Row = (
        &'static str,
        &'static str,
        Option<(&'static str, &'static str)>,
    );

    #[test]
    fn a_function_is_read_out_of_every_value_that_names_one() {
        // The rows differ along the SPELLING of the value: the bare ARN a data source's
        // `lambdaFunctionArn` carries, the version and alias ARNs an `authorizerUri` may be,
        // another partition, and the values that name no function at all.
        let cases: [Row; 8] = [
            (
                "a data source's bare function ARN",
                "arn:aws:lambda:us-east-1:000000000000:function:orders",
                Some((
                    "arn:aws:lambda:us-east-1:000000000000:function:orders",
                    "us-east-1",
                )),
            ),
            (
                "an authorizer's version ARN",
                "arn:aws:lambda:eu-west-2:000000000000:function:gate:3",
                Some((
                    "arn:aws:lambda:eu-west-2:000000000000:function:gate:3",
                    "eu-west-2",
                )),
            ),
            (
                "an authorizer's alias ARN",
                "arn:aws:lambda:ap-south-1:000000000000:function:gate:live",
                Some((
                    "arn:aws:lambda:ap-south-1:000000000000:function:gate:live",
                    "ap-south-1",
                )),
            ),
            (
                "a value in another partition",
                "arn:aws-us-gov:lambda:us-gov-west-1:000000000000:function:orders",
                Some((
                    "arn:aws-us-gov:lambda:us-gov-west-1:000000000000:function:orders",
                    "us-gov-west-1",
                )),
            ),
            (
                "a function ARN embedded in a path",
                "arn:aws:apigateway:us-west-2:lambda:path/2015-03-31/functions/\
                 arn:aws:lambda:us-west-2:000000000000:function:gate/invocations",
                Some((
                    "arn:aws:lambda:us-west-2:000000000000:function:gate",
                    "us-west-2",
                )),
            ),
            (
                "an ARN of another vendor",
                "arn:aws:states:us-east-1:000000000000:stateMachine:orders",
                None,
            ),
            (
                "a function ARN with no Region field",
                "arn:aws:lambda::000000000000:function:orders",
                None,
            ),
            ("a value that is not an ARN at all", "orders", None),
        ];
        for (case, value, expected) in cases {
            let actual = function_target(value);
            let expected = expected.map(|(arn, region)| FunctionTarget {
                arn: arn.to_string(),
                region: region.to_string(),
            });
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    #[test]
    fn a_run_is_failed_only_when_lambda_signalled_an_error() {
        let cases: [(&str, Option<&str>, bool); 4] = [
            ("a handler that returned", None, false),
            ("a handler that raised", Some("Unhandled"), true),
            ("a handled error Lambda reports", Some("Handled"), true),
            ("an empty signal, which signals nothing", Some(""), false),
        ];
        for (case, signal, expected) in cases {
            let ran = Ran {
                payload: Vec::new(),
                function_error: signal.map(str::to_string),
            };
            assert_eq!(
                ran.failed(),
                expected,
                "case {case:?}: expected failed {expected}, got {}",
                ran.failed()
            );
        }
    }

    #[test]
    fn the_class_and_sentence_of_a_raised_error_are_the_runtimes_own() {
        /// One row: what a failing assertion calls it, the payload the run answered with, the
        /// signal Lambda set for it, and the `(class, sentence)` the error is reported under.
        type Raised = (&'static str, &'static str, Option<&'static str>, (&'static str, &'static str));

        // The rows differ along what the error document carries: both members, the class
        // alone, and neither — where the signal Lambda set is all that is known.
        let cases: [Raised; 4] = [
            (
                "a raised exception, class and text",
                r#"{"errorType":"ValueError","errorMessage":"no such item","stackTrace":[]}"#,
                Some("Unhandled"),
                ("ValueError", "no such item"),
            ),
            (
                "an error document with no text",
                r#"{"errorType":"KeyError"}"#,
                Some("Unhandled"),
                ("KeyError", "KeyError"),
            ),
            (
                "a payload that is no error document",
                "\"boom\"",
                Some("Unhandled"),
                ("Unhandled", "Unhandled"),
            ),
            (
                "a payload that is not a document at all",
                "not json",
                Some("Handled"),
                ("Handled", "Handled"),
            ),
        ];
        for (case, payload, signal, expected) in cases {
            let ran = Ran {
                payload: payload.as_bytes().to_vec(),
                function_error: signal.map(str::to_string),
            };
            let actual = ran.raised();
            assert_eq!(
                (actual.0.as_str(), actual.1.as_str()),
                expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
