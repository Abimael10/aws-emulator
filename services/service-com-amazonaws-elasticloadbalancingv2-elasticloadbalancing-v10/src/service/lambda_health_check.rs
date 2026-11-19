// SPDX-License-Identifier: CC0-1.0

//! The HEALTH CHECK of a `lambda` TARGET GROUP: the invocation one check of such a group is,
//! and the verdict the function's own answer decides.
//!
//! Every other target type is checked by a CONNECTION to an address on a port, which is what
//! [`crate::state::host_counts`] observes against the account's own inventory. A `lambda`
//! registration has no address and no port — `TargetGroup$TargetType` registers "a single
//! Lambda function as a target", and the load balancer invokes that function instead of
//! connecting to it — so one check of it is an INVOCATION, made over the in-process transport
//! into the Lambda this emulator mounts ([`super::lambda_target`] owns that contract: the
//! event, the grant and the client). The function therefore RUNS, with every effect its run
//! has — the records of the run reaching the log group it writes to among them — exactly as
//! it does for a request a client sent.
//!
//! Only a group whose health checks are ON is checked at all: "if the target type is lambda,
//! health checks are disabled by default but can be enabled"
//! (`CreateTargetGroup$HealthCheckEnabled`), and a group whose checks are off reports its
//! registrations `unavailable` rather than a health
//! ([`crate::state::target_health::TargetGroupRecord::health_of`]). A check is made on the
//! group's OWN cadence (`HealthCheckIntervalSeconds`) and never more often: an invocation is a
//! run of the function, so a look taken between two checks must not make one.
//!
//! # What the answer decides
//!
//! The check succeeds while the status the function's returned object names is one the target
//! group's `Matcher` counts as success — "[HTTP/HTTPS health checks] The HTTP or gRPC codes to
//! use when checking for a successful response from a target" — which such a group carries
//! like any other HTTP-semantic check ([`crate::state::matcher_admits`]). It FAILS for an
//! answer that is no answer to the check: a run that ended in a function error, a payload that
//! is no response object or names no readable status, a status outside the matcher, an answer
//! that does not arrive within the group's `HealthCheckTimeoutSeconds` ("the amount of time, in
//! seconds, during which no response from a target means a failed health check"), and an
//! invocation the function's own resource policy admits no load balancer to make — Lambda
//! answering that nothing admits the call is an answer about this target, and nothing answers
//! a check that is never made.
//!
//! Nothing is decided on what this emulator could not carry: an invocation no Lambda of this
//! emulator served, or one whose call the transport failed, observes NOTHING, and a
//! registration nothing was observed about keeps the health it has — the discipline every fact
//! this service does not own is read under ([`crate::state::network`]).

use ::smithy_server_runtime::EmbeddedEmulator;

use crate::state::{MatcherRecord, TargetGroupRecord};

/// What one health check of a `lambda` registration invokes, and what the answer to it is
/// judged against — read out of the store under its own guard, so the invocation is made with
/// no lock of this service's held (as a forwarded request's is,
/// [`super::lambda_target::Forwarded`]).
#[derive(Debug, Clone)]
pub(crate) struct Probe {
    /// The ARN of the function the checked registration names.
    pub(crate) function_arn: String,
    /// The target group that registration belongs to, which the event names as the resource
    /// the check belongs to.
    pub(crate) target_group_arn: String,
    /// Whether that group exchanges ARRAYS of header and query-string values with the
    /// function ([`super::lambda_target::multi_value_headers`]).
    pub(crate) multi_value_headers: bool,
    /// The destination the check requests: the group's own `HealthCheckPath`.
    pub(crate) path: String,
    /// How long the check waits for an answer: the group's own `HealthCheckTimeoutSeconds`
    /// ([`crate::state::health_check_timeout`]). No answer within it is a failed check.
    pub(crate) timeout: ::std::time::Duration,
    /// The codes that group counts as a successful response.
    pub(crate) matcher: MatcherRecord,
}

/// The health check of the registration of `function_arn` in `group`, or `None` where that
/// registration is checked by no invocation at all.
///
/// `None` for a group that is not a `lambda` one — those are checked by a connection
/// ([`crate::state::host_counts`]) — for one whose health checks are off, and for one whose
/// check carries no destination to request or no code to compare the answer against: a check
/// that requests nothing, or accepts no response as success, cannot be evaluated at all, which
/// is the pair `CreateTargetGroup` and `ModifyTargetGroup` refuse to store apart
/// ([`crate::state::check_layer_7_health_check`]).
pub(crate) fn probe(
    group: &TargetGroupRecord,
    target_group_arn: &str,
    function_arn: &str,
) -> Option<Probe> {
    if !group.is_lambda() || !group.health_check.enabled {
        return None;
    }
    Some(Probe {
        function_arn: function_arn.to_string(),
        target_group_arn: target_group_arn.to_string(),
        multi_value_headers: super::lambda_target::multi_value_headers(group),
        path: group.health_check.path.clone()?,
        timeout: crate::state::health_check_timeout(group),
        matcher: group.health_check.matcher.clone()?,
    })
}

/// Perform ONE health check: invoke the function the probe names with the health-check event,
/// and answer whether the status it returned is one the group counts as success.
///
/// `None` is "nothing was observed", which fails no check: an invocation this emulator could
/// not carry at all says nothing about the target (see the module docs).
pub(crate) async fn observed(callback: &EmbeddedEmulator, probe: &Probe) -> Option<bool> {
    // The Region the invocation is made in is the function's own, which its ARN names — the
    // ARN a registration of a `lambda` target group is written as.
    let region = ::service_common::arn::region_of(
        &probe.function_arn,
        super::lambda_target::LAMBDA_ARN_NAMESPACE,
    )?;
    let payload = ::serde_json::to_vec(&super::lambda_target::health_check_event(
        &probe.target_group_arn,
        &probe.path,
        probe.multi_value_headers,
    ))
    .ok()?;
    let client = super::lambda_target::lambda_client_over(callback, region);
    // The grant that admits the invocation is the FUNCTION's own, narrowed to the target group
    // the check belongs to — the same grant a forwarded request is invoked under. A check
    // Lambda answers that nothing admits is a check nothing answers.
    if !super::lambda_target::invocation_admitted(
        &client,
        &probe.function_arn,
        &probe.target_group_arn,
    )
    .await
    {
        ::tracing::info!(
            function = %probe.function_arn,
            target_group = %probe.target_group_arn,
            "the registered function admits this load balancer no invocation for this target \
             group, so its health check was not answered"
        );
        return Some(false);
    }
    let answered = client
        .invoke()
        .function_name(&probe.function_arn)
        // The check is judged on what the run answered, so the invocation is the synchronous
        // one: "RequestResponse (default) – Invoke the function synchronously"
        // (`Invoke$InvocationType`).
        .invocation_type(::aws_sdk_lambda::types::InvocationType::RequestResponse)
        .payload(::aws_sdk_lambda::primitives::Blob::new(payload))
        .send();
    // The check waits the group's own `HealthCheckTimeoutSeconds` for the answer, and a check
    // nothing answered within it is a FAILED check — which is also what keeps one slow function
    // from holding this pass for the whole of its own `Timeout`.
    let answered = match ::tokio::time::timeout(probe.timeout, answered).await {
        Ok(answered) => answered,
        Err(_) => {
            ::tracing::info!(
                function = %probe.function_arn,
                timeout_seconds = probe.timeout.as_secs(),
                "the health check of this Lambda target was not answered within the target \
                 group's timeout"
            );
            return Some(false);
        }
    };
    let answered = match answered {
        Ok(answered) => answered,
        Err(error) => {
            if super::lambda_target::refused_by_lambda(&error) {
                // Lambda refusing the call is an answer ABOUT this target — a function it
                // holds no longer cannot answer a check of it.
                ::tracing::info!(
                    function = %probe.function_arn,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the health check of this Lambda target was refused, so nothing answered it"
                );
                return Some(false);
            }
            // Nothing of the target was observed: the call was not carried at all.
            ::tracing::warn!(
                function = %probe.function_arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the health check of this Lambda target was not made, so it observed nothing"
            );
            return None;
        }
    };
    // "FunctionError: if present, indicates that an error occurred during function execution"
    // — a run that errored returned no response object, so the check was not answered.
    if let Some(signal) = answered.function_error() {
        ::tracing::info!(
            function = %probe.function_arn,
            function_error = %signal,
            "the health check's run of this Lambda target ended in a function error"
        );
        return Some(false);
    }
    let returned = answered
        .payload()
        .map(|payload| payload.as_ref().to_vec())
        .unwrap_or_default();
    match super::lambda_target::answered_status(&returned) {
        Ok(status) => Some(crate::state::matcher_admits(&probe.matcher, status)),
        Err(reason) => {
            ::tracing::info!(
                function = %probe.function_arn,
                detail = %reason,
                "the health check's run of this Lambda target returned no response the contract \
                 admits"
            );
            Some(false)
        }
    }
}
