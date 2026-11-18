// SPDX-License-Identifier: CC0-1.0

//! Making the AUTHENTICATION REQUEST whose outcome `TestIdentityProvider` reports.
//!
//! A server that delegates authentication names a provider in its
//! `IdentityProviderDetails`, and what `TestIdentityProvider` answers is the outcome of
//! an authentication request against THAT provider: `StatusCode` is "the HTTP status
//! code that is the response from your API Gateway or your Lambda function", `Response`
//! is "the response that is returned from your API Gateway or your Lambda function".
//! So the answer can only be read off an invocation that really happened.
//!
//! One of the provider kinds the `IdentityProviderType` enum names IS served inside this
//! emulator: an `AWS_LAMBDA` server's `IdentityProviderDetails.Function` names a Lambda
//! function, and the Lambda emulation mounted beside this service holds it. So the
//! request is made — over the in-process callback, with the documented authentication
//! event — and the invocation's own HTTP status and response payload are what the
//! answer carries. Nothing leaves the process: the function is this emulator's own.
//!
//! The other two provider kinds point OUTSIDE: `API_GATEWAY` at a customer-run REST
//! endpoint and `AWS_DIRECTORY_SERVICE` at a directory, neither of which this emulator
//! runs. For those [`authenticate`] answers `None`, and the caller reports the
//! no-response answer `crate::state::identity_provider_test_message` explains — no
//! status and no body are invented for a request nothing sent.

use ::serde_json::{json, Value};

use crate::state as st;

/// The ARN namespace, and the endpoint prefix, of AWS Lambda.
const LAMBDA_ARN_NAMESPACE: &str = "lambda";

/// The longest this operation waits for the provider's function to answer.
///
/// The bound that really governs the run is the function's OWN configured timeout,
/// which AWS Lambda enforces and answers the invocation with ("the maximum allowed value
/// is 900 seconds", `FunctionConfiguration$Timeout`). This ceiling sits a minute above
/// that maximum, so it never pre-empts a function AWS Lambda would still be running,
/// while an invocation that is never answered at all still leaves this operation able to
/// answer its caller.
const INVOCATION_CEILING: ::std::time::Duration = ::std::time::Duration::from_secs(960);

/// The authentication request a test makes, as the operation's input names it.
///
/// Every member is read from the REQUEST: `UserName` and the server's own id are always
/// there, while `UserPassword`, `SourceIp` and `ServerProtocol` are optional ("the
/// `ServerProtocol`, `SourceIp`, and `UserPassword` are all optional"), and a member the
/// request did not name is left out of the event rather than invented.
#[derive(Debug, Clone, Copy)]
pub struct AuthenticationRequest<'a> {
    pub server_id: &'a str,
    pub user_name: &'a str,
    pub user_password: Option<&'a str>,
    pub source_ip: Option<&'a str>,
    pub protocol: Option<&'a str>,
}

/// What the provider answered, as `TestIdentityProvider`'s output reports it.
#[derive(Debug, Clone)]
pub struct ProviderAnswer {
    /// The HTTP status the invocation answered.
    pub status_code: i32,
    /// The provider's own response body, verbatim; `None` when it returned none.
    pub response: Option<String>,
    /// "A message that indicates whether the test was successful or not": EMPTY for a
    /// provider that answered without reporting an error of its own, which is what the
    /// operation documents an empty string for; otherwise the error the provider
    /// reported, or the fixed text for an authentication request that got no answer.
    pub message: String,
}

impl ProviderAnswer {
    /// The answer for an authentication request that reached no provider: no HTTP status
    /// ([`st::IDENTITY_PROVIDER_NO_RESPONSE_STATUS`]), no body, and the fixed text that
    /// says the request was not answered.
    fn unanswered() -> Self {
        Self {
            status_code: st::IDENTITY_PROVIDER_NO_RESPONSE_STATUS,
            response: None,
            message: st::IDENTITY_PROVIDER_NO_ANSWER.to_string(),
        }
    }
}

/// Make the authentication request `request` against the provider `endpoint` names, when
/// it is one this emulator serves, and answer what it answered.
///
/// `None` for a provider this emulator does not run — every type other than `AWS_LAMBDA`,
/// and an `AWS_LAMBDA` server whose details name no function at all — so the caller
/// reports the no-response answer rather than an outcome nothing produced.
///
/// Call it with this service's state lock RELEASED: nothing here reads or writes this
/// service's state, and holding the lock across the invocation would make every other
/// request of this service wait on the provider's function.
pub async fn authenticate(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    identity_provider_type: &str,
    endpoint: &str,
    request: &AuthenticationRequest<'_>,
) -> Option<ProviderAnswer> {
    if identity_provider_type != st::IDENTITY_PROVIDER_AWS_LAMBDA || endpoint.is_empty() {
        return None;
    }
    // The function is addressed in the Region its own ARN names, so a provider in
    // another Region is reached through that Region's Lambda endpoint; an ARN carrying
    // no Region label falls back to the Region the request was signed for.
    let region = ::service_common::arn::region_of(endpoint, LAMBDA_ARN_NAMESPACE)
        .unwrap_or(request_region);
    let invocation = lambda_client(ctx, region)
        .invoke()
        .function_name(endpoint)
        // The answer IS what the function returns, so the invocation is synchronous.
        .invocation_type(::aws_sdk_lambda::types::InvocationType::RequestResponse)
        .payload(::aws_sdk_lambda::primitives::Blob::new(
            authentication_event(request).to_string().into_bytes(),
        ))
        .send();
    let answered = match ::tokio::time::timeout(INVOCATION_CEILING, invocation).await {
        Ok(answered) => answered,
        Err(_) => {
            ::tracing::warn!(
                ceiling_secs = INVOCATION_CEILING.as_secs(),
                "the identity provider's Lambda function did not answer the authentication \
                 request, so the test reports no status for it"
            );
            return Some(ProviderAnswer::unanswered());
        }
    };
    let output = match answered {
        Ok(output) => output,
        // Lambda refused the invocation — the server names a function it does not hold,
        // or one it cannot run — so no provider answered, and no answer is invented.
        Err(error) => {
            ::tracing::warn!(
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the identity provider's Lambda function could not be invoked, so the test \
                 reports no status for it"
            );
            return Some(ProviderAnswer::unanswered());
        }
    };
    Some(ProviderAnswer {
        // "The HTTP status code is in the 200 range for a successful request. For the
        // RequestResponse invocation type, this status code is 200" (`Invoke`): the
        // status the authentication request was answered with.
        status_code: output.status_code(),
        // "Payload: the response from the function, or an error object" — the provider's
        // own body, reported verbatim.
        response: output
            .payload()
            .map(|payload| String::from_utf8_lossy(payload.as_ref()).to_string())
            .filter(|body| !body.is_empty()),
        // "FunctionError: if present, indicates that an error occurred during function
        // execution" — the provider's own error indicator, which is what a message about
        // whether the test succeeded reports; empty when the function reported none.
        message: output.function_error().unwrap_or_default().to_string(),
    })
}

/// The event the identity provider's function is invoked with.
///
/// The documented authentication event of a Lambda identity provider: the user name and
/// the password offered, the protocol the authentication is for, the server the user is
/// authenticating to and the source address it is coming from. Every value is the
/// request's own, and a member the request did not name is absent rather than defaulted.
fn authentication_event(request: &AuthenticationRequest<'_>) -> Value {
    let mut event = json!({
        "username": request.user_name,
        "serverId": request.server_id,
    });
    let optional = [
        ("password", request.user_password),
        ("protocol", request.protocol),
        ("sourceIp", request.source_ip),
    ];
    for (member, value) in optional {
        if let Some(value) = value {
            event[member] = Value::String(value.to_string());
        }
    }
    event
}

/// Lambda's client for `region`, over the IN-PROCESS callback into this same emulator:
/// the identity provider's function is one this emulator holds, so the authentication
/// request never leaves the process.
///
/// The transport is in-process, so everything a network client needs and this one cannot
/// have is disabled: retries, timeouts, stalled-stream protection and identity caching
/// each need a sleep implementation the callback transport is not given, and there is
/// nothing behind it to retry.
fn lambda_client(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_lambda::Client {
    let config = ::aws_sdk_lambda::Config::builder()
        .behavior_version(::aws_sdk_lambda::config::BehaviorVersion::latest())
        .region(::aws_sdk_lambda::config::Region::new(region.to_string()))
        .endpoint_url(format!(
            "https://{LAMBDA_ARN_NAMESPACE}.{region}.amazonaws.com"
        ))
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .http_client(ctx.callback().http_client())
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

    /// The event carries the request's own members, and only the ones it named.
    #[test]
    fn the_authentication_event_carries_what_the_request_named() {
        let full = authentication_event(&AuthenticationRequest {
            server_id: "s-0123456789abcdef0",
            user_name: "probe-user",
            user_password: Some("offered"),
            source_ip: Some("10.0.0.4"),
            protocol: Some(st::PROTOCOL_SFTP),
        });
        assert_eq!(
            full,
            json!({
                "username": "probe-user",
                "serverId": "s-0123456789abcdef0",
                "password": "offered",
                "protocol": "SFTP",
                "sourceIp": "10.0.0.4",
            }),
            "case \"every member named\": got {full}"
        );

        let required_only = authentication_event(&AuthenticationRequest {
            server_id: "s-0123456789abcdef0",
            user_name: "probe-user",
            user_password: None,
            source_ip: None,
            protocol: None,
        });
        assert_eq!(
            required_only,
            json!({"username": "probe-user", "serverId": "s-0123456789abcdef0"}),
            "case \"only the required members\": got {required_only}"
        );
    }
}
