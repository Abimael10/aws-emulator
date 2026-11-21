// SPDX-License-Identifier: CC0-1.0

//! The Amazon Route 53 health check a global endpoint's failover policy WATCHES, and the
//! refusal a policy naming one that does not exist earns.
//!
//! "You also create a Amazon Route 53 health check that will tell EventBridge to route
//! events to the secondary Region when an 'unhealthy' state is encountered"
//! (`CreateEndpoint`): the health check is the thing that TRIGGERS failover, so an endpoint
//! whose policy names a health check nobody created describes a failover that can never be
//! evaluated. EventBridge therefore resolves the health check before it writes anything, and
//! refuses the write when Route 53 holds none — which is what the recorded response for such
//! a request carries (HTTP 400 `ValidationException`).
//!
//! The health check is ROUTE 53's resource, so this service does not keep a second opinion
//! about which health checks exist: it ASKS Route 53, through the in-process callback into
//! this same emulator ([`::smithy_server_runtime::OperationContext::callback`]), with that
//! service's own `GetHealthCheck`. So a health check `CreateHealthCheck` wrote resolves and
//! one `DeleteHealthCheck` removed stops resolving, with no copy of either event here.
//!
//! Three answers are kept apart, because a write does something different with each: a
//! health check Route 53 HOLDS lets the write go on, the refusal Route 53 models for a health
//! check it does not hold ([`NO_SUCH_HEALTH_CHECK`]) refuses the write, and anything else —
//! no Route 53 in this process to ask, a transport that carried nothing, an error class that
//! operation does not model — resolves nothing, so the write goes on rather than refusing on
//! evidence this emulator does not have. That is the same reading this service takes of every
//! other store it asks ([`super::iam_state`]).

/// The resource type a Route 53 health check's ARN names it under:
/// `arn:aws:route53:::healthcheck/<id>`, which is the form `Primary$HealthCheck` is modelled
/// with (`pattern ^arn:aws([a-z]|\-)*:route53:::healthcheck/[\-a-z0-9]+$`). The id that
/// follows it is the bare id `GetHealthCheck` addresses a health check by.
const HEALTH_CHECK_RESOURCE_TYPE: &str = "healthcheck";

/// Route 53 serves ONE global endpoint, and a call to it is signed for this Region.
const ROUTE53_SIGNING_REGION: &str = "us-east-1";

/// The endpoint Route 53 publishes that one global surface at, which is the address the
/// in-process client addresses — the request never leaves this process.
const ROUTE53_ENDPOINT: &str = "https://route53.amazonaws.com";

/// The error code Route 53's own `GetHealthCheck` answers for a health check the account does
/// not hold ("no health check exists with the specified ID", `NoSuchHealthCheck`). Any OTHER
/// answer resolves nothing (see the module docs).
const NO_SUCH_HEALTH_CHECK: &str = "NoSuchHealthCheck";

/// What a writer of a global endpoint reports for a failover policy whose health check does
/// not exist: the ARN the request named and the fact that nothing answers for it.
///
/// The sentence the recorded refusal of such a request carries, on the 400
/// `ValidationException` channel this service refuses a request the model cannot judge on. The
/// ARN is taken from the request verbatim — it is the value the caller has to correct — and
/// the rest of the sentence is fixed.
#[must_use]
pub fn no_such_health_check(health_check_arn: &str) -> String {
    format!("A health check with ARN {health_check_arn} does not exist.")
}

/// The refusal a `RoutingConfig` naming a health check Route 53 does not hold earns, or
/// `None` when the health check resolves — and for a write that names no routing policy at
/// all, which leaves the stored one alone and so references nothing new.
///
/// Asked BEFORE this service's own store is locked, so Route 53's store is never reached
/// while this one's guard is held.
pub async fn missing_health_check(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    routing: Option<&super::RoutingConfig>,
) -> Option<String> {
    let health_check_arn = routing?.failover_config.primary.health_check.as_str();
    // The bare id Route 53 addresses the health check by. A value that is no health-check
    // ARN names nothing to resolve: the modelled pattern already decided its form, and
    // refusing here on a value this parse cannot read would refuse a request about a
    // resource nothing was asked for.
    let id = ::service_common::arn::resource_id_of_type(health_check_arn, HEALTH_CHECK_RESOURCE_TYPE)
        .filter(|id| !id.is_empty())?;
    match route53_client(ctx.callback())
        .get_health_check()
        .health_check_id(id)
        .send()
        .await
    {
        Ok(_) => None,
        Err(error) => {
            use ::aws_sdk_route53::error::ProvideErrorMetadata;
            ::tracing::debug!(
                health_check = health_check_arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "Route 53 answered no health check for this failover policy"
            );
            (error.code() == Some(NO_SUCH_HEALTH_CHECK))
                .then(|| no_such_health_check(health_check_arn))
        }
    }
}

/// The in-process Route 53 client this service resolves a health check through.
///
/// The transport is the emulator's own, so there is nothing to retry, time out or watch for a
/// stalled stream — each of those would need a sleep implementation this client is not given
/// — and the credentials the callback hands over are the emulator's own, used directly: the
/// caller's `Authorization` header is never forwarded.
fn route53_client(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
) -> ::aws_sdk_route53::Client {
    let config = ::aws_sdk_route53::Config::builder()
        .behavior_version(::aws_sdk_route53::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_route53::config::Region::new(
            ROUTE53_SIGNING_REGION.to_string(),
        ))
        .endpoint_url(ROUTE53_ENDPOINT)
        .retry_config(::aws_sdk_route53::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_route53::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_route53::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_route53::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_route53::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the refusal names the health-check ARN the request carried, and nothing else
    /// about the request.
    ///
    /// The rows span the forms the modelled pattern admits: a hex id, a hyphenated one, and
    /// the partition the ARN may name.
    #[test]
    fn the_refusal_names_the_health_check_arn_the_request_carried() {
        for (case, arn) in [
            ("a hex id", "arn:aws:route53:::healthcheck/0123456789abcdef"),
            ("a hyphenated id", "arn:aws:route53:::healthcheck/health-check-7"),
            (
                "another partition",
                "arn:aws-us-gov:route53:::healthcheck/11111111-1111-1111-1111-111111111111",
            ),
        ] {
            let message = no_such_health_check(arn);
            let expected = format!("A health check with ARN {arn} does not exist.");
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }
}
