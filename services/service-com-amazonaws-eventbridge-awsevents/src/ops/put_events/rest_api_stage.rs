// SPDX-License-Identifier: CC0-1.0

//! Delivering a matched event to an API GATEWAY REST API STAGE target: the signed HTTP
//! request EventBridge makes at the deployed stage the target's ARN names, and the retries
//! the target's `RetryPolicy` bounds.
//!
//! A target of the `execute-api` service names the INVOKE ENDPOINT of one method of one
//! deployed stage — `arn:{partition}:execute-api:{region}:{account}:{api-id}/{stage}/{METHOD}/{resource-path}`,
//! the form the `execute-api:Invoke` action is granted on — so delivering to it is making
//! THAT request: the ARN's own verb at that stage's invoke URL, carrying what the target
//! receives as the body. The target's `HttpParameters` are what the caller contributes to it
//! ("the parameters needed for you to contribute to the HTTP request"): each
//! `PathParameterValues` entry fills one `*` of the ARN's resource path in order
//! ([`crate::state::invocation::filled_path`]), and the header and query-string parameters
//! are sent as the request's own, with a value naming a place in the event replaced by what
//! the event carries there
//! ([`crate::state::invocation::resolved_http_parameters`]).
//!
//! EventBridge invokes an API Gateway target AS THE ROLE the target names — "for EC2
//! instances, Kinesis data streams, Step Functions state machines and API Gateway APIs,
//! EventBridge relies on IAM roles that you specify in the `RoleARN` argument in
//! `PutTargets`" — so the role's trust and its `execute-api:Invoke` grant on the target's own
//! ARN decide the delivery before any request is made ([`super::deliver`] asks both), and the
//! request itself is SIGNED, because a stage whose method is authorized by IAM admits no
//! unsigned caller. The signature is the emulator's own in-process one
//! ([`::service_common::in_process_sigv4`]), under the `execute-api` signing name AWS
//! publishes the invoke endpoint under.
//!
//! The request goes through the IN-PROCESS emulator transport and nothing else
//! ([`smithy_server_runtime::EmbeddedEmulator::http_connector`]), so it reaches only what
//! this emulator serves itself: no socket is opened and no name is resolved. An invoke URL
//! this emulator serves nothing at — an api it does not hold, a stage with no deployment —
//! answers the transport's own refusal, which is an attempt that reached no endpoint rather
//! than an answer from one.
//!
//! What ONE attempt comes to is classified exactly as an api-destination attempt is
//! ([`super::api_destination`]): a 2xx is the delivery, a 4xx is the stage REFUSING the event
//! and is not tried again, and a 5xx or an attempt nothing answered is tried again until one
//! of the target's two retry bounds is met ([`crate::state::retry_bounds`]), after which the
//! event goes to the target's dead-letter queue carrying the status the stage answered.
//!
//! WHICH condition that dead letter names is read off the ANSWER the request got
//! ([`answer_condition`]): a `403` is the identity the request was made under not being
//! permitted to invoke the method, which is [`crate::state::NO_PERMISSIONS`], and every other
//! unsuccessful answer is the endpoint not serving the request, which is
//! [`crate::state::CONNECTION_FAILURE`] — the condition an attempt NOTHING answered carries as
//! well, since such an attempt leaves no status to read one off. The sentence is the same
//! either way: the status, the request this delivery made, and the service, status, code and
//! request id of the answer ([`stage_status_failure`]).
//!
//! So the grant the target's role lacks is reported as what it DOES to the request. The
//! documents in force on that role are read before any request is made — the shared decision
//! [`super::role_refusal`] takes for every target kind — and a role they do not allow
//! [`INVOKE_ACTION`] on the target's own ARN is reported as the `403` the invoke endpoint
//! answers such an identity ([`invoke_authorization_refusal`]), rather than as a sentence about
//! the evaluation this emulator took it from: the answer is what a caller of the dead-letter
//! queue can observe, and it is the same class of answer whichever document withheld the
//! grant.

use super::*;

/// The ARN namespace of an API Gateway INVOKE endpoint, which is the service AWS publishes
/// the `execute-api:Invoke` action and the stage invoke URL under — not the control plane's
/// (`apigateway`), whose ARNs name an api's configuration rather than its invoke endpoint.
pub(super) const EXECUTE_API_ARN_NAMESPACE: &str = "execute-api";

/// The IAM action the delivery performs, which the target's role has to be allowed on the
/// target's own ARN.
pub(super) const INVOKE_ACTION: &str = "execute-api:Invoke";

/// The status an invoke endpoint answers a request whose identity may not perform
/// [`INVOKE_ACTION`] on the method it addressed.
const FORBIDDEN_STATUS: u16 = 403;

/// The content type EventBridge sends a target that takes an HTTP request, which is what
/// decides WHICH of the method's integration request templates renders the call.
const STAGE_CONTENT_TYPE: &str = "application/json; charset=utf-8";

/// The reserved path prefix a REST API's invoke endpoint is reached at ON THIS EMULATOR,
/// ahead of the api's own id: a client cannot resolve the host AWS publishes for a stage, so
/// the api id travels in the path instead. It is the addressing the data plane serving those
/// stages reads, and the one every reader of this emulator announces a stage at.
const LOCAL_INVOKE_PREFIX: &str = "_aws/execute-api";

/// How the failure below names the service the request was made to.
const INVOKE_SERVICE_NAME: &str = "api_gateway";

/// The response header an invoke endpoint names the request it answered in, which is where
/// the id the failure below reports comes from.
const RESPONSE_REQUEST_ID: &str = "x-amzn-requestid";

/// The time ONE attempt at a stage is given.
///
/// This emulator's OWN bound rather than a limit AWS publishes: it is a CAP on an attempt,
/// never a wait, so an attempt that answers sooner costs what it costs. The same cap an
/// attempt at an api destination is given ([`API_DESTINATION_TIMEOUT`]), so one delivery
/// cannot hold the publisher longer than the other.
const STAGE_TIMEOUT: ::std::time::Duration = API_DESTINATION_TIMEOUT;

/// What the invoke endpoint a target ARN names addresses: which api, which stage, which verb
/// and which resource path within it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Stage {
    /// The api's own id, which is the first label of its invoke host.
    api_id: String,
    /// The stage the request is served by, which is the first segment of the invoke path.
    stage: String,
    /// The verb the ARN names, which is the method the request is made with.
    method: String,
    /// The resource path within the stage, as the ARN spells it — its `*` wildcards still in
    /// place, with a leading `/` and empty for the api's own root.
    path: String,
}

impl Stage {
    /// The invoke URL of this stage, with each `*` of its resource path filled from `values`.
    ///
    /// AWS reaches a stage at `{api-id}.execute-api.{region}.amazonaws.com/{stage}/{path}` —
    /// the endpoint `RestApi$disableExecuteApiEndpoint` names ("the default
    /// `https://{api_id}.execute-api.{region}.amazonaws.com` endpoint"). Nothing resolves
    /// that host on the machine this emulator runs on, so the address a stage is announced
    /// and reached at HERE is the emulator's own endpoint carrying in the PATH what it
    /// cannot carry in the host: the reserved data-plane prefix and the api's id, then the
    /// stage and the resource path. That is the one address this delivery can name and a
    /// client can read back.
    fn invoke_url(&self, endpoint: &str, values: &[String]) -> String {
        let path = crate::state::invocation::filled_path(&self.path, values);
        format!(
            "{}/{LOCAL_INVOKE_PREFIX}/{}/{}{path}",
            endpoint.trim_end_matches('/'),
            self.api_id,
            self.stage
        )
    }
}

/// The verbs an invoke ARN names ONE request by: the HTTP methods RFC 9110 defines and the
/// one RFC 5789 adds, which is also what `Method$httpMethod` admits for a method of an api.
/// The `*` an `execute-api:Invoke` GRANT may carry in that position names every verb rather
/// than one to make a request with, and it is a valid HTTP token, so the verbs are checked
/// against this set rather than for being a token at all.
const INVOCABLE_VERBS: [::http::Method; 8] = [
    ::http::Method::GET,
    ::http::Method::HEAD,
    ::http::Method::POST,
    ::http::Method::PUT,
    ::http::Method::DELETE,
    ::http::Method::OPTIONS,
    ::http::Method::PATCH,
    ::http::Method::TRACE,
];

/// The invoke endpoint `arn` addresses, or `None` when it names none.
///
/// The ARN's resource is `{api-id}/{stage}/{METHOD}` and then the resource path, so an ARN
/// stating fewer than those three names no request to make: nothing says which api, which
/// stage or which verb it would be. The verb has to be one of [`INVOCABLE_VERBS`], and the
/// api id and the stage have to be single segments.
///
/// Every part of the ARN that is interpolated into the path this request is made at — the api
/// id, the stage, and each segment of the resource path — has to NAME A SEGMENT of it
/// ([`names_a_segment`]), so an ARN that would climb out of the stage it spells, or end the
/// path rather than continue it, names no request either: the request this delivery makes is
/// the one the stage serves, not another path of whatever is serving it.
pub(super) fn addressed_stage(arn: &str) -> Option<Stage> {
    let parsed = ::service_common::arn::parse(arn)?;
    if parsed.vendor != EXECUTE_API_ARN_NAMESPACE {
        return None;
    }
    let mut segments = parsed.resource.splitn(4, '/');
    let api_id = segments.next()?;
    let stage = segments.next()?;
    let method = segments.next()?;
    let path = segments.next().unwrap_or_default();
    // The api id and the stage are each one segment of that path, held to the same rule its
    // own segments are.
    if !names_a_segment(api_id) || !names_a_segment(stage) {
        return None;
    }
    if !addresses_inside_the_stage(path) {
        return None;
    }
    let verb = ::http::Method::from_bytes(method.as_bytes()).ok()?;
    if !INVOCABLE_VERBS.contains(&verb) {
        return None;
    }
    Some(Stage {
        api_id: api_id.to_string(),
        stage: stage.to_string(),
        method: verb.to_string(),
        path: match path.is_empty() {
            true => String::new(),
            false => format!("/{path}"),
        },
    })
}

/// Whether the resource path `path` of an invoke ARN names a place inside the stage it
/// follows.
///
/// A path of the stage is a sequence of real segments ([`names_a_segment`]). An EMPTY path is
/// the stage's own root, which is a place inside it.
fn addresses_inside_the_stage(path: &str) -> bool {
    if path.is_empty() {
        return true;
    }
    path.split('/').all(names_a_segment)
}

/// Whether `segment` names ONE segment of the path this request is made at.
///
/// A segment is a real segment: an EMPTY one, a `.` or a `..` names no place of its own — the
/// first two name the place they are in and the third the one above it, so a path carrying
/// them does not address what it spells — and neither does one carrying a query or fragment
/// mark, an escape or a backslash, which would end the path rather than continue it. The `*`
/// an ARN's path may carry is a wildcard the target's values fill
/// ([`crate::state::invocation::filled_path`]) and a segment like any other here.
///
/// Every part of an invoke ARN that reaches the URL goes through this: the api id, the stage
/// and each segment of the resource path, so no one of them can take the request out of the
/// place the ARN names while another is held to it.
fn names_a_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment.contains(['?', '#', '%', '\\'])
}

/// Invoke a REST API stage target: the ARN's own request at that stage's invoke URL,
/// carrying the event, retried within the target's `RetryPolicy`.
///
/// The classification, the retry bounds and the dead letter are the ones the module docs
/// state; the outcome is what [`super::deliver`] records and dead-letters.
pub(super) async fn invoke_rest_api_stage(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    endpoint: &str,
    region: &str,
    handover: &Handover<'_>,
    body: &str,
    spent: &mut Spent,
    budget: &mut DeliveryBudget,
) -> crate::state::InvocationOutcome {
    let target = handover.target;
    let Some(stage) = addressed_stage(&target.arn) else {
        return crate::state::InvocationOutcome::NotAttempted(
            "the target ARN names no api, stage and method to make a request to".to_string(),
        );
    };
    let parameters = resolved_parameters(handover);
    let bounds = crate::state::retry_bounds::RetryBounds::of(
        target.maximum_retry_attempts,
        target.maximum_event_age_in_seconds,
    );
    // How old the EVENT already is where this delivery begins, which is what the age bound is
    // read against.
    let age = ::std::time::SystemTime::now()
        .duration_since(handover.published)
        .unwrap_or_default();
    let transport = callback.http_connector();
    // WHERE the request goes, built once for every attempt this delivery makes.
    let uri = request_uri(endpoint, &stage, parameters.as_ref());
    // What the LAST attempt came to, which is what the dead letter reports.
    let mut last: Option<StageAttempt> = None;
    // How many attempts were made; one more than the retries the bounds are counted in.
    let mut attempted: i32 = 0;
    // WHICH of the target's bounds ended the delivery, left `None` by a bound of this
    // emulator's own: a condition the target's retry policy did not produce must not be
    // reported as one it did.
    let mut exhausted: Option<&'static str> = None;
    loop {
        // One yield per attempt, so a retried delivery stays interruptible.
        ::tokio::task::yield_now().await;
        // What the whole REQUEST has left to wait on endpoints, which is what bounds the
        // product of the fan-out and the per-attempt timeout.
        if !budget.may_wait_on_an_endpoint() {
            ::tracing::warn!(
                target = %target.arn,
                bound = ?MAX_ENDPOINT_WAIT_PER_REQUEST,
                "this request reached the time it may spend waiting on endpoints; this \
                 delivery makes no further attempt"
            );
            break;
        }
        attempted += 1;
        // Reported as the attempt is MADE, so the rule's attempt counter counts the
        // attempts this delivery really made whatever it ends as
        // ([`super::count_invocation`]).
        spent.attempts = attempted;
        let started = ::std::time::Instant::now();
        let attempt = attempt_stage(
            callback,
            &transport,
            &uri,
            region,
            &stage,
            parameters.as_ref(),
            body,
        )
        .await;
        budget.charge_endpoint_wait(started.elapsed());
        // The stage ANSWERED and refused: a 4xx is not retried, so the delivery fails on
        // this attempt carrying that answer.
        if let StageAttempt::Answered { status, request_id } = &attempt {
            if endpoint_refused(*status) {
                let message = stage_status_failure(&stage, &uri, *status, request_id);
                // Retry accounting is reported only for a delivery that was RETRIED.
                spent.retries = (attempted > 1).then(|| Retries {
                    attempts: attempted - 1,
                    exhausted: None,
                });
                return crate::state::InvocationOutcome::Failed {
                    code: answer_condition(*status).to_string(),
                    message,
                };
            }
        }
        if matches!(attempt, StageAttempt::Delivered) {
            return crate::state::InvocationOutcome::Invoked;
        }
        last = Some(attempt);
        // Both of the target's bounds are asked around EVERY retryable failure, so the
        // delivery stops on whichever is met first and the dead letter names it.
        let made = attempted - 1;
        if let Some(condition) = bounds.exhausted_after(made, age) {
            exhausted = Some(condition);
            break;
        }
        // This emulator's own bound on the real requests one delivery makes, the guard
        // behind the target's own bounds above.
        if made >= crate::state::retry_bounds::MAX_RETRIES_PER_DELIVERY {
            ::tracing::warn!(
                target = %target.arn,
                bound = crate::state::retry_bounds::MAX_RETRIES_PER_DELIVERY,
                "this delivery reached the retries one makes in this emulator; neither bound \
                 of the target's retry policy was met"
            );
            break;
        }
    }
    // The condition and the sentence the LAST attempt earns, which is what it was answered
    // where it was answered at all.
    let (code, message) = match &last {
        // The stage's own answer, in the words AWS reports a target endpoint's answer with.
        Some(StageAttempt::Answered { status, request_id }) => (
            answer_condition(*status),
            stage_status_failure(&stage, &uri, *status, request_id),
        ),
        // Nothing answered: there is no status to read a condition off, so this is the
        // endpoint not serving the request, and what the attempts came to is reported.
        Some(StageAttempt::Unreached(detail)) => (
            crate::state::CONNECTION_FAILURE,
            stage_failure(&uri, detail, attempted),
        ),
        // Nothing was attempted at all, which only the request's endpoint-wait budget leaves
        // behind: the loop above attempts before it decides anything else.
        Some(StageAttempt::Delivered) | None => (
            crate::state::CONNECTION_FAILURE,
            stage_failure(
                &uri,
                "this request reached the time this emulator lets one spend waiting on \
                 endpoints",
                attempted,
            ),
        ),
    };
    spent.retries = Some(Retries {
        attempts: (attempted - 1).max(0),
        exhausted,
    });
    crate::state::InvocationOutcome::Failed {
        code: code.to_string(),
        message,
    }
}

/// The target's own HTTP parameters, with every value that names a place in the EVENT resolved
/// against it: the event as published, not the input this delivery reshaped, because a
/// parameter names a value of the event.
fn resolved_parameters(handover: &Handover<'_>) -> Option<HttpParameters> {
    let parameters = handover.target.http_parameters.as_ref()?;
    let event = ::serde_json::from_str::<::serde_json::Value>(handover.rendered)
        .unwrap_or(::serde_json::Value::Null);
    Some(crate::state::invocation::resolved_http_parameters(
        parameters, &event,
    ))
}

/// WHERE one delivery's request goes: the stage's invoke url with the target's path values
/// filling the ARN's wildcards and its query-string parameters on it.
///
/// The one address this delivery names — the attempts are made at it, and a delivery refused
/// before any attempt reports the request it would have made at it
/// ([`invoke_authorization_refusal`]).
fn request_uri(endpoint: &str, stage: &Stage, parameters: Option<&HttpParameters>) -> String {
    let path_values = parameters
        .and_then(|parameters| parameters.path_parameter_values.clone())
        .unwrap_or_default();
    let query: ::std::collections::BTreeMap<String, String> = parameters
        .and_then(|parameters| parameters.query_string_parameters.clone())
        .unwrap_or_default()
        .into_iter()
        .collect();
    crate::state::invocation::with_query_parameters(
        &stage.invoke_url(endpoint, &path_values),
        &query,
    )
}

/// The condition EventBridge names on the dead letter of a delivery the invoke endpoint
/// answered `status` to.
///
/// The ANSWER is what it is read off: `403` is the one status an invoke endpoint answers a
/// request whose identity may not perform [`INVOKE_ACTION`] on the method it addressed, so
/// that delivery failed for want of a permission ([`crate::state::NO_PERMISSIONS`]); every
/// other unsuccessful status is the endpoint not serving the request
/// ([`crate::state::CONNECTION_FAILURE`]).
fn answer_condition(status: u16) -> &'static str {
    match status {
        FORBIDDEN_STATUS => crate::state::NO_PERMISSIONS,
        _ => crate::state::CONNECTION_FAILURE,
    }
}

/// Why the documents in force on the target's role bar this delivery — worded as the ANSWER
/// the request such a role makes is given — or `None` when nothing of the kind can be shown.
///
/// The decision is the shared one every target kind takes about the call it makes
/// ([`super::role_refusal`]): the documents in force on the role either allow
/// [`INVOKE_ACTION`] on the target's own ARN or they do not, and nothing is refused on
/// evidence this emulator does not have. What is this module's own is the REPORT. An identity
/// that may not invoke the method is answered [`FORBIDDEN_STATUS`] by the invoke endpoint, so
/// such a delivery fails the way every answered attempt does: the condition that status
/// carries ([`answer_condition`]) and the sentence wording it ([`stage_status_failure`]),
/// naming the request this delivery addressed the stage with. A refusal of the step BEFORE the
/// request — a role EventBridge cannot assume at all — is reported as itself, because no
/// request is addressed at all then; so is one for a target ARN that names no request to make.
pub(super) fn invoke_authorization_refusal(
    iam: Option<&::engine::ServiceState<crate::state::IamState>>,
    role_arn: Option<&str>,
    handover: &Handover<'_>,
    attempt: &crate::state::DeliveryAttempt<'_>,
    endpoint: &str,
) -> Option<(String, String)> {
    let (code, message) = super::role_refusal(iam, role_arn, handover.rule_arn, attempt)?;
    if code != crate::state::NO_PERMISSIONS {
        return Some((code, message));
    }
    let Some(stage) = addressed_stage(&handover.target.arn) else {
        return Some((code, message));
    };
    let uri = request_uri(endpoint, &stage, resolved_parameters(handover).as_ref());
    // The id of the answer: an attempt that was MADE carries the id the endpoint named the
    // request it answered by ([`RESPONSE_REQUEST_ID`]), and this one carries the id minted for
    // the answer it stands for, as every refusal this emulator words for an answer does.
    let request_id = ::service_common::query_error_envelope::new_request_id();
    Some((
        answer_condition(FORBIDDEN_STATUS).to_string(),
        stage_status_failure(&stage, &uri, FORBIDDEN_STATUS, &request_id),
    ))
}

/// What ONE attempt at a stage came to.
enum StageAttempt {
    /// The stage answered 2xx: the event was delivered.
    Delivered,
    /// The stage ANSWERED and did not take the event, with the status it answered and the id
    /// of the request it answered, which the message below reports. A 4xx is the stage
    /// refusing the event and is not tried again ([`endpoint_refused`]); any other non-2xx
    /// answer is.
    Answered { status: u16, request_id: String },
    /// NOTHING answered: the attempt reached no endpoint, was not answered within the time
    /// one attempt is given, or is not a request this target renders. Tried again like any
    /// other retryable failure.
    Unreached(String),
}

/// What a dead letter reports for a delivery whose STAGE answered and did not take the
/// event: the wording a dead letter of this class carries, whose variable parts are the
/// status it answered, the REQUEST this delivery made (its verb and the url it was made to),
/// the reason phrase of that status and the id of the request the stage answered.
///
/// Every variable part is read off this delivery and its answer rather than written here:
/// the url is the one [`Stage::invoke_url`] built, the reason phrase is the HTTP status's own
/// (`::http::StatusCode::canonical_reason`), and the request id is the one the stage's
/// response named itself ([`RESPONSE_REQUEST_ID`]) — so what the message states is what the
/// endpoint answered, and no value of one delivery is part of the template.
fn stage_status_failure(stage: &Stage, url: &str, status: u16, request_id: &str) -> String {
    let reason = ::http::StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or_default();
    format!(
        "Got status code \"{status}\" while trying to invoke \"{} {url}\". \
         (Service: {INVOKE_SERVICE_NAME}; Status Code: {status}; Error Code: {reason}; \
         Request ID: {request_id}; Proxy: null)",
        stage.method
    )
}

/// What a failed stage delivery nothing answered is recorded and dead-lettered with: the
/// request it was making, what the last attempt came to, and how many attempts were made.
fn stage_failure(url: &str, detail: &str, attempts: i32) -> String {
    format!("Got no answer from \"{url}\" after {attempts} attempt(s): {detail}")
}

/// Make one attempt at the stage, bounded by [`STAGE_TIMEOUT`], and classify it.
///
/// What the request CARRIES: the ARN's own verb, the url the caller built from the ARN and
/// the target's parameters, the target's header parameters, the content type a target taking
/// an HTTP request is sent, the event as the body — and the emulator's own SigV4 signature
/// over all of it, written last so it covers every header the request ends up carrying.
async fn attempt_stage(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    transport: &::aws_smithy_runtime_api::client::http::SharedHttpConnector,
    uri: &str,
    region: &str,
    stage: &Stage,
    parameters: Option<&HttpParameters>,
    body: &str,
) -> StageAttempt {
    use ::aws_smithy_runtime_api::client::http::HttpConnector;

    let mut builder = ::http::Request::builder()
        .method(stage.method.as_str())
        .uri(uri)
        .header(::http::header::CONTENT_TYPE, STAGE_CONTENT_TYPE);
    for (name, value) in parameters
        .and_then(|parameters| parameters.header_parameters.as_ref())
        .into_iter()
        .flatten()
    {
        builder = builder.header(name, value);
    }
    // Every value here comes from the stored target, so a parameter that is not a header or a
    // path that is not a URI fails HERE rather than panicking.
    let mut request = match builder.body(body.as_bytes().to_vec()) {
        Ok(request) => request,
        Err(error) => {
            return StageAttempt::Unreached(format!(
                "the target's own parameters do not render a request ({error})"
            ))
        }
    };
    // Signed as the emulator's own in-process caller, under the signing name the invoke
    // endpoint is reached by: a stage whose method is authorized by IAM serves no unsigned
    // request, so an unsigned attempt would be refused by the endpoint rather than delivered.
    if let Err(error) = ::service_common::in_process_sigv4::sign_callback_request(
        &mut request,
        EXECUTE_API_ARN_NAMESPACE,
        region,
        callback,
    )
    .await
    {
        return StageAttempt::Unreached(format!("the request could not be signed ({error})"));
    }
    let (parts, payload) = request.into_parts();
    let signed = ::http::Request::from_parts(
        parts,
        ::aws_smithy_types::body::SdkBody::from(::bytes::Bytes::from(payload)),
    );
    let request = match ::aws_smithy_runtime_api::http::Request::try_from(signed) {
        Ok(request) => request,
        Err(error) => {
            return StageAttempt::Unreached(format!(
                "the invoke url does not render a request ({error})"
            ))
        }
    };
    match ::tokio::time::timeout(STAGE_TIMEOUT, transport.call(request)).await {
        // Nothing answered inside the time one attempt is given.
        Err(_elapsed) => StageAttempt::Unreached(format!(
            "the stage did not answer within {} seconds",
            STAGE_TIMEOUT.as_secs()
        )),
        Ok(Err(error)) => StageAttempt::Unreached(format!(
            "the stage could not be reached ({})",
            ::aws_smithy_types::error::display::DisplayErrorContext(&error)
        )),
        Ok(Ok(response)) => {
            let status = response.status().as_u16();
            // A refusal the TRANSPORT itself made is not an answer from the stage: this
            // emulator serves nothing at that invoke url, so the attempt reached no endpoint
            // at all — the same class as a stage that never answers.
            if response
                .body()
                .bytes()
                .is_some_and(crate::state::is_transport_refusal)
            {
                return StageAttempt::Unreached(
                    "no endpoint answered: this emulator serves nothing at that invoke url"
                        .to_string(),
                );
            }
            match status {
                200..=299 => StageAttempt::Delivered,
                // The id the stage named the request it answered by, which the failure this
                // becomes reports: the endpoint states it on every response it serves, and
                // nothing else here knows it.
                _ => StageAttempt::Answered {
                    status,
                    request_id: response
                        .headers()
                        .get(RESPONSE_REQUEST_ID)
                        .unwrap_or_default()
                        .to_string(),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The endpoint the rows below address, standing for the base URL this emulator is
    /// reached at.
    const ENDPOINT: &str = "https://127.0.0.1:9297";

    /// RULE: a target ARN of the `execute-api` service names the api, the stage, the verb and
    /// the resource path one request is made to, and an ARN that states fewer than those
    /// names no request to make.
    #[test]
    fn the_target_arn_names_the_request_the_delivery_makes() {
        /// (case, the ARN, the url the delivery addresses with no path value at all)
        type Row<'a> = (&'a str, &'a str, Option<&'a str>);
        // The rows differ along how many segments the resource path has, whether it carries a
        // wildcard, the verb, and what puts an ARN outside the form: another service, a
        // resource stating no verb, and a verb that is no HTTP method.
        let rows: &[Row<'_>] = &[
            (
                "one path segment",
                "arn:aws:execute-api:eu-west-1:000000000000:a1b2c3d4e5/live/POST/orders",
                Some("https://127.0.0.1:9297/_aws/execute-api/a1b2c3d4e5/live/orders"),
            ),
            (
                "a nested path carrying a wildcard",
                "arn:aws:execute-api:eu-west-1:000000000000:a1b2c3d4e5/live/POST/orders/*",
                Some("https://127.0.0.1:9297/_aws/execute-api/a1b2c3d4e5/live/orders/*"),
            ),
            (
                "the stage's own root, which the resource path is empty for",
                "arn:aws:execute-api:eu-west-1:000000000000:a1b2c3d4e5/live/GET",
                Some("https://127.0.0.1:9297/_aws/execute-api/a1b2c3d4e5/live"),
            ),
            (
                "another verb",
                "arn:aws:execute-api:eu-west-1:000000000000:a1b2c3d4e5/beta/PUT/items/17",
                Some("https://127.0.0.1:9297/_aws/execute-api/a1b2c3d4e5/beta/items/17"),
            ),
            (
                "another service's ARN",
                "arn:aws:apigateway:eu-west-1::/restapis/a1b2c3d4e5/stages/live",
                None,
            ),
            (
                "an ARN naming no verb",
                "arn:aws:execute-api:eu-west-1:000000000000:a1b2c3d4e5/live",
                None,
            ),
            (
                "the wildcard verb an invoke GRANT may carry, which names no one request",
                "arn:aws:execute-api:eu-west-1:000000000000:a1b2c3d4e5/live/*/orders",
                None,
            ),
            (
                "a verb that is a valid HTTP token and no HTTP method",
                "arn:aws:execute-api:eu-west-1:000000000000:a1b2c3d4e5/live/FETCH/orders",
                None,
            ),
            (
                "an ARN naming no api",
                "arn:aws:execute-api:eu-west-1:000000000000:/live/POST/orders",
                None,
            ),
            (
                // The path has to name a place INSIDE the stage: one climbing out of it
                // addresses a path the stage does not serve, so the ARN names no request.
                "a resource path climbing out of the stage",
                "arn:aws:execute-api:eu-west-1:000000000000:a1b2c3d4e5/live/POST/../../admin",
                None,
            ),
            (
                "a resource path naming the place it is in",
                "arn:aws:execute-api:eu-west-1:000000000000:a1b2c3d4e5/live/POST/orders/./17",
                None,
            ),
            (
                "a resource path carrying an empty segment",
                "arn:aws:execute-api:eu-west-1:000000000000:a1b2c3d4e5/live/POST/orders//17",
                None,
            ),
            (
                // A query mark ends the path rather than continuing it, so such a path is
                // not a place in the stage either — and neither is an escaped delimiter.
                "a resource path carrying a query mark",
                "arn:aws:execute-api:eu-west-1:000000000000:a1b2c3d4e5/live/POST/orders?x=1",
                None,
            ),
            (
                "a resource path carrying an escape",
                "arn:aws:execute-api:eu-west-1:000000000000:a1b2c3d4e5/live/POST/%2e%2e/admin",
                None,
            ),
            (
                "a stage carrying a query mark",
                "arn:aws:execute-api:eu-west-1:000000000000:a1b2c3d4e5/live?x=1/POST/orders",
                None,
            ),
            (
                // The api id and the stage are segments of that path too, so neither may be
                // one that names the place above it.
                "a stage that is the place above the api",
                "arn:aws:execute-api:eu-west-1:000000000000:a1b2c3d4e5/../POST/orders",
                None,
            ),
            (
                "an api id that is the place above the prefix",
                "arn:aws:execute-api:eu-west-1:000000000000:../live/POST/orders",
                None,
            ),
            (
                "a stage naming the place it is in",
                "arn:aws:execute-api:eu-west-1:000000000000:a1b2c3d4e5/./POST/orders",
                None,
            ),
        ];
        for (case, arn, expected) in rows {
            let addressed = addressed_stage(arn);
            let url = addressed
                .as_ref()
                .map(|stage| stage.invoke_url(ENDPOINT, &[]));
            assert_eq!(
                url.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {url:?}"
            );
        }
    }

    /// RULE: each `PathParameterValues` entry fills one `*` of the ARN's resource path, in
    /// order.
    #[test]
    fn the_path_values_fill_the_wildcards_of_the_resource_path() {
        /// (case, the ARN's resource path, the values, the path of the url)
        type Row<'a> = (&'a str, &'a str, &'a [&'a str], &'a str);
        let rows: &[Row<'_>] = &[
            ("no wildcard and no value", "orders", &[], "/live/orders"),
            (
                "one wildcard, one value",
                "orders/*",
                &["w-1"],
                "/live/orders/w-1",
            ),
            (
                "two wildcards, two values, filled in order",
                "*/items/*",
                &["orders", "17"],
                "/live/orders/items/17",
            ),
            (
                "two wildcards, one value: nothing says what the second stands for",
                "*/items/*",
                &["orders"],
                "/live/orders/items/*",
            ),
            (
                "one wildcard, two values: the spare fills nothing",
                "orders/*",
                &["w-1", "spare"],
                "/live/orders/w-1",
            ),
            (
                // A value fills the ONE place its wildcard holds, so a path separator in it
                // travels as part of that place rather than opening another segment.
                "a value carrying a path separator",
                "orders/*",
                &["17/items"],
                "/live/orders/17%2Fitems",
            ),
            (
                "a value climbing out of the stage",
                "orders/*",
                &["../../admin"],
                "/live/orders/..%2F..%2Fadmin",
            ),
            (
                "a value carrying an escaped separator",
                "orders/*",
                &["%2f..%2fadmin"],
                "/live/orders/%252f..%252fadmin",
            ),
            (
                "a value carrying the query and fragment marks",
                "orders/*",
                &["17?x=1#f"],
                "/live/orders/17%3Fx=1%23f",
            ),
        ];
        for (case, path, values, expected) in rows {
            let arn =
                format!("arn:aws:execute-api:eu-west-1:000000000000:a1b2c3d4e5/live/POST/{path}");
            let stage = addressed_stage(&arn)
                .unwrap_or_else(|| panic!("case {case:?}: the ARN names a stage"));
            let owned: Vec<String> = values.iter().map(|value| (*value).to_string()).collect();
            let url = stage.invoke_url(ENDPOINT, &owned);
            let tail = url
                .strip_prefix(&format!("{ENDPOINT}/{LOCAL_INVOKE_PREFIX}/a1b2c3d4e5"))
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                tail, *expected,
                "case {case:?}: expected {expected:?}, got {tail:?}"
            );
        }
    }
}
