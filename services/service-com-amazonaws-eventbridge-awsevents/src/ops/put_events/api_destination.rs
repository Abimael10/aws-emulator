// SPDX-License-Identifier: CC0-1.0

//! Delivering a matched event to an API DESTINATION target: the HTTP request
//! EventBridge makes at the destination's `InvocationEndpoint`, how one attempt
//! at it is classified, and the retries the target's `RetryPolicy` bounds.
//!
//! It is the one target kind whose delivery is RETRIED here — "retry attempts
//! continue until either the maximum number of attempts is made or until the
//! duration of the `MaximumEventAgeInSeconds` is met"
//! ([`crate::state::retry_bounds`] decides which of the two ends a delivery) —
//! so it carries the retry bookkeeping the dead letter reports, which is why it
//! sits beside the rest of [`super`] rather than inside it.

use super::*;

/// The time ONE attempt at an API destination's endpoint is given.
///
/// AWS documents a 5 second client execution timeout for an API destination
/// endpoint, after which the attempt is abandoned and retried within the
/// target's retry policy. It is a CAP on an attempt, never a wait: an attempt
/// that answers sooner costs what it costs.
pub(super) const API_DESTINATION_TIMEOUT: ::std::time::Duration = ::std::time::Duration::from_secs(5);

/// The content type AWS sends an API destination's invocation with.
pub(super) const API_DESTINATION_CONTENT_TYPE: &str = "application/json; charset=utf-8";
/// The user agent AWS sends an API destination's invocation with.
pub(super) const API_DESTINATION_USER_AGENT: &str = "Amazon/EventBridge/ApiDestinations";

/// Invoke an API destination target: an HTTP request to the endpoint the stored
/// destination holds, carrying the event, retried within the target's
/// `RetryPolicy`.
///
/// The rule AWS follows: EventBridge makes the destination's `HttpMethod`
/// request to its `InvocationEndpoint` with the event as the body, bounded by a
/// client execution timeout ([`API_DESTINATION_TIMEOUT`]); an answer in the 2xx
/// range is the delivery, a 4xx is the endpoint REFUSING it and is not tried
/// again, and a 5xx or an attempt that never answered is tried again until one
/// of the target's two retry bounds is met — "retry attempts continue until
/// either the maximum number of attempts is made or until the duration of the
/// `MaximumEventAgeInSeconds` is met" ([`crate::state::retry_bounds`]) — after
/// which the event goes to the target's dead-letter queue like any other
/// undeliverable one (see [`deliver`]), carrying the bound that ended it.
///
/// The request goes through the IN-PROCESS emulator transport and nothing else
/// ([`smithy_server_runtime::EmbeddedEmulator::http_connector`]), so an endpoint
/// a caller stored reaches only what this emulator serves itself: no socket is
/// opened and no name is resolved. An endpoint this emulator serves nothing at
/// answers the transport's own refusal, which is an attempt that reached no
/// endpoint rather than an answer from one — the retryable class, exactly as an
/// endpoint that never answers is.
///
/// The connection's authorization material and both sides' HTTP parameters ARE
/// merged into the request ([`attempt_api_destination`]).
pub(super) async fn invoke_api_destination(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    handover: &Handover<'_>,
    body: &str,
    spent: &mut Spent,
    budget: &mut DeliveryBudget,
) -> crate::state::InvocationOutcome {
    let target = handover.target;
    let Some(destination) = target.api_destination.as_ref() else {
        // The target names a destination that is not there to take the event,
        // which is the condition a resource that is absent carries
        // ([`delivery_condition`]).
        return crate::state::InvocationOutcome::Failed {
            code: crate::state::NO_RESOURCE.to_string(),
            message: format!(
                "Target {} names no api destination this account holds, so no request was made \
                 to one.",
                target.arn
            ),
        };
    };
    // "An API destination ... is ACTIVE only while the connection it uses is
    // authorized": an inactive destination is not invoked, so no request is made
    // and the event is not delivered.
    if !destination.active {
        return crate::state::InvocationOutcome::Failed {
            code: crate::state::UNKNOWN_CONDITION.to_string(),
            message: format!(
                "Api destination {} is not ACTIVE, so no request was made to its invocation \
                 endpoint.",
                destination.name
            ),
        };
    }
    let bounds = crate::state::retry_bounds::RetryBounds::of(
        target.maximum_retry_attempts,
        target.maximum_event_age_in_seconds,
    );
    // How old the EVENT already is where this delivery begins, which is what the
    // age bound is read against: an event published with a `Time` in the past is
    // that much closer to its bound before the first attempt is made.
    let age = ::std::time::SystemTime::now()
        .duration_since(handover.published)
        .unwrap_or_default();
    let transport = callback.http_connector();
    // The target's own parameters, with every value that names a place in the
    // EVENT resolved against it ([`crate::state::invocation::resolved_http_parameters`]):
    // what a parameter contributes is a value of the event, not of the input this
    // delivery reshaped, and it is resolved ONCE for every attempt this delivery
    // makes.
    let event = ::serde_json::from_str::<::serde_json::Value>(handover.rendered)
        .unwrap_or(::serde_json::Value::Null);
    let parameters = target
        .http_parameters
        .as_ref()
        .map(|parameters| crate::state::invocation::resolved_http_parameters(parameters, &event));
    // What the LAST attempt came to, which is what the dead letter reports.
    let mut last: Option<Attempt> = None;
    // How many attempts were made; one more than the retries, which is what the
    // retry bounds are counted in.
    let mut attempted: i32 = 0;
    // WHICH of the target's bounds ended the delivery, left `None` by a bound of
    // this emulator's own: a condition the target's retry policy did not produce
    // must not be reported as one it did.
    let mut exhausted: Option<&'static str> = None;
    loop {
        // One yield per attempt, so a retried delivery stays interruptible.
        ::tokio::task::yield_now().await;
        // What the whole REQUEST has left to wait on endpoints, which is what
        // bounds the product of the fan-out and the per-attempt timeout.
        if !budget.may_wait_on_an_endpoint() {
            ::tracing::warn!(
                destination = %destination.name,
                bound = ?MAX_ENDPOINT_WAIT_PER_REQUEST,
                "this request reached the time it may spend waiting on api destination \
                 endpoints; this delivery makes no further attempt"
            );
            break;
        }
        attempted += 1;
        // Reported as the attempt is MADE, so what the rule's attempt counter
        // counts is the attempts this delivery really made whatever it ends as
        // ([`super::count_invocation`]).
        spent.attempts = attempted;
        let started = ::std::time::Instant::now();
        let attempt =
            attempt_api_destination(&transport, destination, parameters.as_ref(), body).await;
        budget.charge_endpoint_wait(started.elapsed());
        // The endpoint ANSWERED and refused: "a 4xx response is not retried", so
        // the delivery fails on this attempt, under the condition the target's
        // own answer gives it and carrying that answer.
        if let Attempt::Answered { status, payload } = &attempt {
            if endpoint_refused(*status) {
                let message =
                    crate::state::retry_bounds::api_destination_status_failure(*status, payload);
                // Retry accounting is reported only for a delivery that was
                // RETRIED: a refusal on the first attempt retried nothing.
                spent.retries = (attempted > 1).then(|| Retries {
                    attempts: attempted - 1,
                    exhausted: None,
                });
                return crate::state::InvocationOutcome::Failed {
                    code: crate::state::INVALID_PARAMETER.to_string(),
                    message,
                };
            }
        }
        if matches!(attempt, Attempt::Delivered) {
            return crate::state::InvocationOutcome::Invoked;
        }
        last = Some(attempt);
        // Both of the target's bounds are asked around EVERY retryable failure,
        // so the delivery stops on whichever is met first and the dead letter
        // names it.
        let made = attempted - 1;
        if let Some(condition) = bounds.exhausted_after(made, age) {
            exhausted = Some(condition);
            break;
        }
        // This emulator's own bound on the real requests one delivery makes. It
        // is derived from the widest age bound the model admits, so a target
        // within the model is stopped by one of its own bounds above and this is
        // the guard rather than the rule.
        if made >= crate::state::retry_bounds::MAX_RETRIES_PER_DELIVERY {
            ::tracing::warn!(
                destination = %destination.name,
                bound = crate::state::retry_bounds::MAX_RETRIES_PER_DELIVERY,
                "this delivery reached the retries one makes in this emulator; neither bound of \
                 the target's retry policy was met"
            );
            break;
        }
    }
    let message = match &last {
        // The endpoint's own answer, in AWS's wording: the status it answered is
        // readable back out of the dead letter.
        Some(Attempt::Answered { status, payload }) => {
            crate::state::retry_bounds::api_destination_status_failure(*status, payload)
        }
        // Nothing answered, for which AWS publishes no wording: what the
        // attempts came to is reported instead.
        Some(Attempt::Unreached(detail)) => {
            api_destination_failure(destination, detail, attempted)
        }
        // Nothing was attempted at all, which only the request's endpoint-wait
        // budget leaves behind: the loop above attempts before it decides
        // anything else.
        Some(Attempt::Delivered) | None => api_destination_failure(
            destination,
            "this request reached the time this emulator lets one spend waiting on api \
             destination endpoints",
            attempted,
        ),
    };
    spent.retries = Some(Retries {
        attempts: (attempted - 1).max(0),
        exhausted,
    });
    crate::state::InvocationOutcome::Failed {
        code: crate::state::UNKNOWN_CONDITION.to_string(),
        message,
    }
}

/// Whether the status an endpoint answered is the endpoint REFUSING the event
/// rather than a failure the delivery may try again: "a 4xx response is not
/// retried", and every other non-2xx answer is.
pub(super) fn endpoint_refused(status: u16) -> bool {
    (400..500).contains(&status)
}

/// The message a failed api-destination delivery is recorded and dead-lettered
/// with: which destination, what the last attempt came to, and how many attempts
/// were made.
pub(super) fn api_destination_failure(destination: &ApiDestination, detail: &str, attempts: i32) -> String {
    format!(
        "Api destination {} did not accept the event after {attempts} attempt(s) at {}: {detail}",
        destination.name, destination.endpoint
    )
}

/// What ONE attempt at an API destination's endpoint came to.
pub(super) enum Attempt {
    /// The endpoint acknowledged the event.
    Delivered,
    /// The endpoint ANSWERED and did not acknowledge it, with the status it
    /// answered and the body that came with it — which together are what AWS
    /// reports for such an attempt
    /// ([`crate::state::retry_bounds::api_destination_status_failure`]). A 4xx
    /// is the endpoint refusing the event and is not retried
    /// ([`endpoint_refused`]); any other non-2xx answer is.
    Answered { status: u16, payload: String },
    /// NOTHING answered: the attempt did not reach an endpoint, was not answered
    /// within the timeout, or was not a request this destination renders. Tried
    /// again like any other retryable failure.
    Unreached(String),
}

/// How much of an endpoint's answer the dead letter carries.
///
/// This emulator's OWN bound: the payload goes into one SQS message attribute,
/// whose value Amazon SQS bounds along with the rest of the message, so an
/// endpoint answering a large body cannot make the dead letter unsendable. Held
/// to characters so the cut never lands inside one.
pub(super) const MAX_REPORTED_PAYLOAD_CHARS: usize = 1024;

/// What an endpoint answered, as the dead letter carries it: its body as text,
/// held to [`MAX_REPORTED_PAYLOAD_CHARS`].
pub(super) fn reported_payload(payload: &[u8]) -> String {
    let text = String::from_utf8_lossy(payload);
    match text.char_indices().nth(MAX_REPORTED_PAYLOAD_CHARS) {
        None => text.into_owned(),
        Some((end, _)) => text[..end].to_string(),
    }
}

/// Make one attempt, bounded by [`API_DESTINATION_TIMEOUT`], and classify it.
///
/// What the request CARRIES is what EventBridge sends an API destination: the
/// event as the body, the content type and user agent AWS sends it with, the
/// connection's authorization, and the connection's and the target's header,
/// query-string, path and body parameters merged with the connection's value
/// winning ([`crate::state::invocation::api_destination_invocation`]).
///
/// `parameters` are the TARGET's own `HttpParameters` with every value that names
/// a place in the event already resolved against it, which the caller does once
/// for every attempt it makes
/// ([`crate::state::invocation::resolved_http_parameters`]).
///
/// `transport` is the in-process emulator connector and the ONLY thing this
/// request is handed to: the endpoint text decides which emulated service
/// receives it, and a URL naming anything this emulator does not serve is
/// answered by the transport's own refusal rather than by a connection.
pub(super) async fn attempt_api_destination(
    transport: &::aws_smithy_runtime_api::client::http::SharedHttpConnector,
    destination: &ApiDestination,
    parameters: Option<&HttpParameters>,
    body: &str,
) -> Attempt {
    use ::aws_smithy_runtime_api::client::http::HttpConnector;

    let method = match ::http::Method::from_bytes(destination.method.as_bytes()) {
        Ok(method) => method,
        Err(_) => {
            return Attempt::Unreached(format!(
                "its stored HttpMethod {:?} is not an HTTP method",
                destination.method
            ))
        }
    };
    let invocation = crate::state::invocation::api_destination_invocation(
        &destination.endpoint,
        &destination.secret,
        parameters,
        body,
        API_DESTINATION_CONTENT_TYPE,
        API_DESTINATION_USER_AGENT,
    );
    // Every value here comes from the stored destination, its connection and
    // the stored target, so an endpoint or a parameter that is not a URI or a
    // header fails HERE rather than panicking. Each header is written ONCE:
    // the content type and user agent AWS sends are part of the merge above, so
    // a parameter naming one of them replaces it rather than adding a second.
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(&invocation.uri);
    for (name, value) in &invocation.headers {
        builder = builder.header(name, value);
    }
    let request = builder
        .body(::aws_smithy_types::body::SdkBody::from(
            ::bytes::Bytes::from(invocation.body.clone()),
        ))
        .map_err(|error| error.to_string())
        .and_then(|request| {
            ::aws_smithy_runtime_api::http::Request::try_from(request)
                .map_err(|error| error.to_string())
        });
    let request = match request {
        Ok(request) => request,
        Err(error) => {
            return Attempt::Unreached(format!(
                "its stored invocation endpoint does not render a request ({error})"
            ))
        }
    };
    match ::tokio::time::timeout(API_DESTINATION_TIMEOUT, transport.call(request)).await {
        // Nothing answered inside the client execution timeout.
        Err(_elapsed) => Attempt::Unreached(format!(
            "the endpoint did not answer within {} seconds",
            API_DESTINATION_TIMEOUT.as_secs()
        )),
        Ok(Err(error)) => Attempt::Unreached(format!(
            "the endpoint could not be reached ({})",
            ::aws_smithy_types::error::display::DisplayErrorContext(&error)
        )),
        Ok(Ok(response)) => {
            let status = response.status().as_u16();
            // A refusal the TRANSPORT itself made is not an answer from an
            // endpoint: the in-process emulator serves nothing at this URL (or
            // more than one service claims it), so the attempt reached no
            // endpoint at all — which is the same class as an endpoint that
            // never answers, not a 4xx the endpoint sent back.
            if response
                .body()
                .bytes()
                .is_some_and(crate::state::is_transport_refusal)
            {
                return Attempt::Unreached(
                    "no endpoint answered: this emulator serves nothing at that invocation \
                     endpoint"
                        .to_string(),
                );
            }
            match status {
                200..=299 => Attempt::Delivered,
                // The endpoint answered and did not acknowledge the event: the
                // status and the body it answered with are what the delivery
                // reports, whether it retries ([`endpoint_refused`]) or not.
                _ => Attempt::Answered {
                    status,
                    payload: reported_payload(response.body().bytes().unwrap_or_default()),
                },
            }
        }
    }
}
