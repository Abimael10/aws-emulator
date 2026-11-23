// SPDX-License-Identifier: CC0-1.0

//! A `forward` to a `LAMBDA` TARGET GROUP: how a request a client sent to a Lattice service
//! is served by INVOKING the function the registration names, and what the service answers
//! for a function answer that breaks the response contract.
//!
//! `TargetGroupType` gives the registration — "indicates targets in this target group are
//! Lambda" — and `Target$id` gives what it holds: "if the target group type is `LAMBDA`, this
//! is the ARN of a Lambda function". So a `forward` to such a group names ONE function, and
//! the request is made against it THROUGH THIS EMULATOR, over the in-process transport
//! `OperationContext::callback` hands an operation: no connection is opened, and what the
//! client is answered with is what the function's own run produced.
//!
//! # What the function is handed
//!
//! The service hands the function ONE event describing the request it received, in the
//! structure the target group's own configuration selects: "the version of the event structure
//! that your Lambda function receives … The default is `V1`"
//! (`TargetGroupConfig$lambdaEventStructureVersion`). The two structures differ in how they
//! spell the request — the V1 one names the method, the raw path, the headers, the query
//! string parameters and the body, each single-valued; [`V2_EVENT`] names its own version,
//! carries the headers and the query string parameters as LISTS, and adds the
//! `requestContext` describing the resources the request travelled through — and every value
//! in both is read off THIS request and this service's own records. Nothing is echoed from a
//! value a test invented: the body travels as text where its bytes are text and
//! base64-encoded with the flag set where they are not, which is the one encoding a JSON
//! document carries bytes in.
//!
//! A member whose value these records do not establish is left out rather than filled in with
//! a guess: the service network the request came through is reported only where the service is
//! associated with exactly one ([`LambdaTarget::service_network_arn`]).
//!
//! # What it must return, and what the service answers instead
//!
//! The returned object is a CONTRACT between the service and its target: a run that errored,
//! a payload that is not a JSON object, one naming no readable status code, and one whose
//! headers or body are not of the shape the contract gives them are each answered
//! [`BAD_GATEWAY_STATUS`] — the service's own answer, with the function's payload NEVER passed
//! through. The invocation itself still happened: the function ran, with whatever effects its
//! run had, and nothing of the registration or of its health changes.
//!
//! No body is invented for that status, exactly as the sibling statuses this plane answers
//! carry none ([`super::data_plane`]): the service's own error page is not a document this
//! emulator holds.

use ::serde_json::{Map, Value};
use ::smithy_server_runtime::OperationContext;

use super::data_plane::{DataPlaneRequest, Served};

/// What the service answers when the function it forwarded the request to returned no
/// response the contract admits — including a run that produced no response at all. The
/// service answers it ITSELF: the payload the function returned is not passed through.
///
/// The bad-gateway class, which is what a proxy answers for a backend response it cannot use;
/// THE EMULATOR'S OWN choice of it, carrying no body for the reason the module header gives.
pub(super) const BAD_GATEWAY_STATUS: u16 = 502;

/// The `lambdaEventStructureVersion` value that selects the SECOND structure; every other
/// value — the `V1` the member defaults to
/// ([`crate::state::DEFAULT_LAMBDA_EVENT_STRUCTURE_VERSION`]) among them — selects the first.
pub(super) const V2_EVENT: &str = "V2";

/// How many bytes of request BODY one invocation of a Lambda target carries.
///
/// THE EMULATOR'S OWN bound, not a limit this service documents: the body becomes the event
/// the function is invoked with, so what one request takes into this process is bounded here
/// rather than by whatever a client chose to send.
pub(super) const MAX_EVENT_BODY_BYTES: usize = 1024 * 1024;

/// What a request whose body is larger than that is answered with: the status HTTP defines for
/// exactly that condition (RFC 9110 section 15.5.14, "Content Too Large"). The request never
/// becomes an event, so nothing is invoked for it.
pub(super) const PAYLOAD_TOO_LARGE_STATUS: u16 = 413;

/// The `version` member the V2 structure states for itself.
const V2_VERSION: &str = "2.0";

/// The members of the V1 event.
const V1_METHOD: &str = "method";
const V1_RAW_PATH: &str = "raw_path";
const V1_HEADERS: &str = "headers";
const V1_QUERY: &str = "query_string_parameters";
const V1_BODY: &str = "body";
const V1_BASE64: &str = "is_base64_encoded";

/// The members of the V2 event.
const V2_VERSION_MEMBER: &str = "version";
const V2_METHOD: &str = "method";
const V2_PATH: &str = "path";
const V2_HEADERS: &str = "headers";
const V2_QUERY: &str = "queryStringParameters";
const V2_BODY: &str = "body";
const V2_BASE64: &str = "isBase64Encoded";
const V2_REQUEST_CONTEXT: &str = "requestContext";
const V2_SERVICE_ARN: &str = "serviceArn";
const V2_SERVICE_NETWORK_ARN: &str = "serviceNetworkArn";
const V2_TARGET_GROUP_ARN: &str = "targetGroupArn";
const V2_REGION: &str = "region";
const V2_TIME_EPOCH: &str = "timeEpoch";

/// The members of the object the function returns.
const STATUS_CODE_MEMBER: &str = "statusCode";
const HEADERS_MEMBER: &str = "headers";
const BODY_MEMBER: &str = "body";
const BASE64_MEMBER: &str = "isBase64Encoded";

/// The ARN namespace a Lambda function's ARN is written under, which is where the Region the
/// invocation is made in is read from.
const LAMBDA_ARN_NAMESPACE: &str = "lambda";

/// Which function a `forward` to a `LAMBDA` target group invokes, and what the event reports
/// about the request's own path through this account's resources.
///
/// Read off the records under the store's lock and carried out of it, so the invocation itself
/// holds no guard of this service.
#[derive(Debug, Clone)]
pub(super) struct LambdaTarget {
    /// `Target$id` as the registration holds it: "the ARN of a Lambda function".
    pub(super) function_arn: String,
    /// The ARN of the target group the request was forwarded to.
    pub(super) target_group_arn: String,
    /// The structure of the event this group's own configuration selects
    /// (`TargetGroupConfig$lambdaEventStructureVersion`).
    pub(super) event_structure_version: String,
    /// The ARN of the service the request was addressed to.
    pub(super) service_arn: String,
    /// The ARN of the service network the request came through, where these records establish
    /// which one that was, and `None` where they do not.
    pub(super) service_network_arn: Option<String>,
}

/// Serve one request by invoking the function `target` names, and answer what the run
/// produced — or the service's own [`BAD_GATEWAY_STATUS`] when it produced nothing the
/// contract admits.
pub(super) async fn served(
    ctx: &OperationContext<'_>,
    target: &LambdaTarget,
    request: &DataPlaneRequest,
) -> Served {
    // The event the function is invoked with CARRIES the request's body, so the bound on one
    // event bounds the request: a body past it is refused before an event is built from it,
    // and nothing is invoked.
    if request.body.len() > MAX_EVENT_BODY_BYTES {
        ::tracing::info!(
            target_group = %target.target_group_arn,
            bytes = request.body.len(),
            "the request's body is larger than one invocation of a Lambda target carries, so \
             the function is not invoked"
        );
        return Served::status(PAYLOAD_TOO_LARGE_STATUS);
    }
    // The Region the invocation is made in is the FUNCTION's own, which its ARN names — the
    // ARN a registration of a `LAMBDA` target group is written as.
    let Some(region) = ::service_common::arn::region_of(&target.function_arn, LAMBDA_ARN_NAMESPACE)
    else {
        ::tracing::warn!(
            target = %target.function_arn,
            "the registered target is not a Lambda function ARN, so no function was invoked"
        );
        return Served::status(BAD_GATEWAY_STATUS);
    };
    let payload = match ::serde_json::to_vec(&event(target, request, ::std::time::SystemTime::now()))
    {
        Ok(payload) => payload,
        Err(error) => {
            ::tracing::warn!(
                error = %error,
                "the forwarded request could not be written as an event, so no function was \
                 invoked"
            );
            return Served::status(BAD_GATEWAY_STATUS);
        }
    };
    let answered = lambda_client(ctx, region)
        .invoke()
        .function_name(&target.function_arn)
        // The client is answered WITH the function's response, so the invocation is the
        // synchronous one: "RequestResponse (default) – Invoke the function synchronously"
        // (`Invoke$InvocationType`).
        .invocation_type(::aws_sdk_lambda::types::InvocationType::RequestResponse)
        .payload(::aws_sdk_lambda::primitives::Blob::new(payload))
        .send()
        .await;
    let answered = match answered {
        Ok(answered) => answered,
        Err(error) => {
            // No invocation was made at all — the target could not be reached — which is the
            // same answer a function response the contract refuses earns, because either way
            // this plane has no usable response to pass on.
            ::tracing::warn!(
                function = %target.function_arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the Lambda target of the forward did not answer the invocation"
            );
            return Served::status(BAD_GATEWAY_STATUS);
        }
    };
    // "FunctionError: if present, indicates that an error occurred during function execution"
    // — a run that errored returned no response object, and the service answers the client
    // itself rather than passing the error document on.
    if let Some(signal) = answered.function_error() {
        ::tracing::info!(
            function = %target.function_arn,
            function_error = %signal,
            "the Lambda target's run ended in a function error, so the service answers the \
             request itself"
        );
        return Served::status(BAD_GATEWAY_STATUS);
    }
    let returned = answered
        .payload()
        .map(|payload| payload.as_ref().to_vec())
        .unwrap_or_default();
    match mapped_response(&returned) {
        Ok(served) => served,
        Err(reason) => {
            ::tracing::info!(
                function = %target.function_arn,
                detail = %reason,
                "the Lambda target returned no response this service's contract admits, so the \
                 service answers the request itself"
            );
            Served::status(BAD_GATEWAY_STATUS)
        }
    }
}

/// The event one forwarded request becomes, in the structure the target group selected.
pub(super) fn event(
    target: &LambdaTarget,
    request: &DataPlaneRequest,
    now: ::std::time::SystemTime,
) -> Value {
    let query = query_pairs(&request.query);
    let headers: Vec<(String, String)> = request.headers.clone();
    let mut event = Map::new();
    if target.event_structure_version.eq_ignore_ascii_case(V2_EVENT) {
        event.insert(V2_VERSION_MEMBER.to_string(), Value::from(V2_VERSION));
        event.insert(V2_PATH.to_string(), Value::from(request.path.clone()));
        event.insert(V2_METHOD.to_string(), Value::from(request.method.clone()));
        event.insert(V2_HEADERS.to_string(), multi_valued(&headers));
        event.insert(V2_QUERY.to_string(), multi_valued(&query));
        carried_body(&mut event, V2_BODY, V2_BASE64, &request.body);
        event.insert(
            V2_REQUEST_CONTEXT.to_string(),
            request_context(target, now),
        );
        return Value::Object(event);
    }
    event.insert(V1_METHOD.to_string(), Value::from(request.method.clone()));
    event.insert(V1_RAW_PATH.to_string(), Value::from(request.path.clone()));
    event.insert(V1_HEADERS.to_string(), single_valued(&headers));
    event.insert(V1_QUERY.to_string(), single_valued(&query));
    carried_body(&mut event, V1_BODY, V1_BASE64, &request.body);
    Value::Object(event)
}

/// What the V2 structure's `requestContext` reports: the resources of this account the request
/// travelled through, the Region the service's own ARN names, and the instant the event was
/// built.
///
/// A member these records do not establish is left out — the service network for a service
/// associated with more than one, and every member about an identity nothing authenticated —
/// rather than being filled in with a value nothing observed.
fn request_context(target: &LambdaTarget, now: ::std::time::SystemTime) -> Value {
    let mut context = Map::new();
    context.insert(
        V2_SERVICE_ARN.to_string(),
        Value::from(target.service_arn.clone()),
    );
    if let Some(service_network_arn) = target.service_network_arn.clone() {
        context.insert(
            V2_SERVICE_NETWORK_ARN.to_string(),
            Value::from(service_network_arn),
        );
    }
    context.insert(
        V2_TARGET_GROUP_ARN.to_string(),
        Value::from(target.target_group_arn.clone()),
    );
    if let Some(region) = ::service_common::arn::region_of(
        &target.service_arn,
        crate::state::ARN_SERVICE,
    ) {
        context.insert(V2_REGION.to_string(), Value::from(region));
    }
    let since_epoch = now
        .duration_since(::std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    // Stated in milliseconds, as text: the member names an instant, and a JSON number would
    // not carry one of this magnitude without a client having to read it as a float.
    context.insert(
        V2_TIME_EPOCH.to_string(),
        Value::from(since_epoch.as_millis().to_string()),
    );
    Value::Object(context)
}

/// Put the request's BODY on the event: the text it is where its bytes are text, and
/// base64-encoded with the flag set where they are not — the one encoding a JSON document
/// carries bytes in.
fn carried_body(event: &mut Map<String, Value>, body_member: &str, flag_member: &str, body: &[u8]) {
    match ::std::str::from_utf8(body) {
        Ok(text) => {
            event.insert(body_member.to_string(), Value::from(text));
            event.insert(flag_member.to_string(), Value::from(false));
        }
        Err(_not_text) => {
            event.insert(
                body_member.to_string(),
                Value::from(::aws_smithy_types::base64::encode(body)),
            );
            event.insert(flag_member.to_string(), Value::from(true));
        }
    }
}

/// The query string as the pairs it carries, in the order it carries them, each
/// percent-decoded the way a form-encoded query is read.
fn query_pairs(query: &str) -> Vec<(String, String)> {
    if query.is_empty() {
        return Vec::new();
    }
    ::form_urlencoded::parse(query.as_bytes())
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect()
}

/// The single-valued form of a set of pairs: an object of one value per name, which carries
/// the LAST value a repeated name was given — the value a single-valued member can hold.
fn single_valued(pairs: &[(String, String)]) -> Value {
    let mut object = Map::new();
    for (name, value) in pairs {
        object.insert(name.clone(), Value::from(value.clone()));
    }
    Value::Object(object)
}

/// The multi-valued form of a set of pairs: an object of a LIST per name, which carries every
/// value a repeated name was given.
fn multi_valued(pairs: &[(String, String)]) -> Value {
    let mut object = Map::new();
    for (name, value) in pairs {
        match object.get_mut(name).and_then(Value::as_array_mut) {
            Some(values) => values.push(Value::from(value.clone())),
            None => {
                object.insert(name.clone(), Value::Array(vec![Value::from(value.clone())]));
            }
        }
    }
    Value::Object(object)
}

/// The response the object a function returned describes, or the reason it describes none.
///
/// Every refusal here is one the SERVICE answers: the object is the contract between the two,
/// so a payload that is not one is not passed through to the client.
pub(super) fn mapped_response(payload: &[u8]) -> Result<Served, String> {
    let returned = match ::serde_json::from_slice::<Value>(payload) {
        Ok(Value::Object(returned)) => returned,
        _ => return Err("the response is not a JSON object".to_string()),
    };
    Ok(Served {
        status: status_of(&returned)?,
        headers: headers_of(&returned)?,
        body: body_of(&returned)?,
    })
}

/// The status the returned object names, refused when it is not a number inside the range a
/// response status can take (RFC 9110 section 15: three digits, 100 through 599) — the member
/// every response has to carry.
fn status_of(returned: &Map<String, Value>) -> Result<u16, String> {
    let named = returned.get(STATUS_CODE_MEMBER).cloned().unwrap_or_default();
    let status = match &named {
        Value::Number(number) => number.as_i64(),
        // A function that states the status as a string states one too: `"200"` is the status
        // 200, which is what a handler written in a dynamically typed runtime produces when it
        // formats the value.
        Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    };
    let status =
        status.ok_or_else(|| format!("the response names no readable {STATUS_CODE_MEMBER}: {named}"))?;
    u16::try_from(status)
        .ok()
        .filter(|status| (100..=599).contains(status))
        .ok_or_else(|| {
            format!("the response names the status {status}, which is no HTTP response status")
        })
}

/// The headers the returned object names, by lower-cased name. A member that is not an object
/// of header values is a response the contract does not admit, and so is a value that is not
/// text: a header field is text.
fn headers_of(returned: &Map<String, Value>) -> Result<Vec<(String, String)>, String> {
    let Some(named) = returned.get(HEADERS_MEMBER) else {
        return Ok(Vec::new());
    };
    if named.is_null() {
        return Ok(Vec::new());
    }
    let Some(object) = named.as_object() else {
        return Err(format!("the response's {HEADERS_MEMBER} is not an object"));
    };
    object
        .iter()
        .map(|(name, value)| {
            header_value(value)
                .map(|value| (name.to_ascii_lowercase(), value))
                .ok_or_else(|| {
                    format!("the response's {HEADERS_MEMBER} names {name:?} as no header value")
                })
        })
        .collect()
}

/// One header value as it is sent: text as itself, and a number or a boolean as its own
/// rendering, which is what a handler setting one means.
fn header_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// The body the returned object names: the text as it is, or the bytes it decodes to when the
/// function marked it base64-encoded. An object naming no body has an empty one, and a body
/// that is not text — or a flag that is not a boolean — is a response the contract does not
/// admit.
fn body_of(returned: &Map<String, Value>) -> Result<Vec<u8>, String> {
    let encoded = match returned.get(BASE64_MEMBER) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(other) => {
            return Err(format!(
                "the response's {BASE64_MEMBER} is not a boolean: {other}"
            ));
        }
    };
    let body = match returned.get(BODY_MEMBER) {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::String(text)) => text.clone(),
        Some(other) => {
            return Err(format!("the response's {BODY_MEMBER} is not text: {other}"));
        }
    };
    if !encoded {
        return Ok(body.into_bytes());
    }
    ::aws_smithy_types::base64::decode(body.trim()).map_err(|error| {
        format!("the response marks its {BODY_MEMBER} base64-encoded and it is not base64: {error}")
    })
}

/// The Lambda client the invocation is made with.
///
/// The transport is the emulator's own, in process, so the call is served by the Lambda this
/// emulator mounts and no connection is opened; the credentials are the ones the callback
/// hands over, which sign this call and are never reported or logged.
fn lambda_client(ctx: &OperationContext<'_>, region: &str) -> ::aws_sdk_lambda::Client {
    let callback = ctx.callback();
    let config = ::aws_sdk_lambda::Config::builder()
        .behavior_version(::aws_sdk_lambda::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_lambda::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://lambda.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client is
        // not given. The WAIT is bounded without one: the run this invocation makes is bounded
        // by the function's own `Timeout` ("the amount of time that Lambda allows a function to
        // run before stopping it"), which Lambda enforces, so the answer comes back however
        // long the handler tries to take.
        .retry_config(::aws_sdk_lambda::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_lambda::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_lambda::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_lambda::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_lambda::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    //! The two pure halves of the contract: the event one request becomes in each structure,
    //! and the response one returned object describes. What the plane SERVES is pinned end to
    //! end in `core/router/tests/all/vpc_lattice_lambda_target_data_plane.rs`.

    use super::*;

    /// The target the rows below forward to.
    fn target(event_structure_version: &str, service_network_arn: Option<&str>) -> LambdaTarget {
        LambdaTarget {
            function_arn: "arn:aws:lambda:us-west-2:000000000000:function:shop".to_string(),
            target_group_arn:
                "arn:aws:vpc-lattice:us-west-2:000000000000:targetgroup/tg-0123456789abcdef0"
                    .to_string(),
            event_structure_version: event_structure_version.to_string(),
            service_arn: "arn:aws:vpc-lattice:us-west-2:000000000000:service/svc-0123456789abcdef0"
                .to_string(),
            service_network_arn: service_network_arn.map(ToString::to_string),
        }
    }

    /// One request for the rows below.
    fn request(method: &str, path: &str, query: &str, body: &[u8]) -> DataPlaneRequest {
        DataPlaneRequest {
            method: method.to_string(),
            path: path.to_string(),
            query: query.to_string(),
            headers: vec![
                ("host".to_string(), "shop-0123456789abcdef0.localhost".to_string()),
                ("x-shelf".to_string(), "front".to_string()),
            ],
            body: body.to_vec(),
        }
    }

    /// RULE: the event carries the request as the client sent it, in the structure the target
    /// group's configuration selects — single-valued members under V1, lists and a
    /// `requestContext` under V2 — and every value in it is read off the request or off this
    /// service's own records.
    ///
    /// The rows span what the rule generalises over: both event structures, a repeated query
    /// parameter, a body of text, no body at all, bytes that are no text, and a service whose
    /// network these records cannot name.
    #[test]
    fn the_event_carries_the_request_in_the_structure_the_group_selected() {
        let at = ::std::time::SystemTime::UNIX_EPOCH + ::std::time::Duration::from_millis(1_500);

        let v1 = event(
            &target(
                crate::state::DEFAULT_LAMBDA_EVENT_STRUCTURE_VERSION,
                Some("arn:aws:vpc-lattice:us-west-2:000000000000:servicenetwork/sn-0123456789abcdef0"),
            ),
            &request("POST", "/orders", "shelf=front&shelf=back", b"a body"),
            at,
        );
        for (member, expected) in [
            (V1_METHOD, Value::from("POST")),
            (V1_RAW_PATH, Value::from("/orders")),
            // The single-valued member carries the LAST value a repeated name was given.
            (V1_QUERY, ::serde_json::json!({ "shelf": "back" })),
            (V1_BODY, Value::from("a body")),
            (V1_BASE64, Value::from(false)),
            (
                V1_HEADERS,
                ::serde_json::json!({
                    "host": "shop-0123456789abcdef0.localhost",
                    "x-shelf": "front",
                }),
            ),
        ] {
            let read = v1.get(member).cloned().unwrap_or(Value::Null);
            assert_eq!(
                read, expected,
                "case \"the V1 event\": expected {member} {expected}, got {read}"
            );
        }
        assert!(
            v1.get(V2_REQUEST_CONTEXT).is_none(),
            "case \"the V1 event\": the V1 structure carries no requestContext, got {v1}"
        );

        let v2 = event(
            &target(V2_EVENT, Some("arn:aws:vpc-lattice:us-west-2:000000000000:servicenetwork/sn-0123456789abcdef0")),
            &request("GET", "/", "shelf=front&shelf=back", &[]),
            at,
        );
        for (member, expected) in [
            (V2_VERSION_MEMBER, Value::from(V2_VERSION)),
            (V2_METHOD, Value::from("GET")),
            (V2_PATH, Value::from("/")),
            // The multi-valued members carry EVERY value a repeated name was given.
            (V2_QUERY, ::serde_json::json!({ "shelf": ["front", "back"] })),
            (
                V2_HEADERS,
                ::serde_json::json!({
                    "host": ["shop-0123456789abcdef0.localhost"],
                    "x-shelf": ["front"],
                }),
            ),
            (V2_BODY, Value::from("")),
            (V2_BASE64, Value::from(false)),
            (
                V2_REQUEST_CONTEXT,
                ::serde_json::json!({
                    "serviceArn":
                        "arn:aws:vpc-lattice:us-west-2:000000000000:service/svc-0123456789abcdef0",
                    "serviceNetworkArn":
                        "arn:aws:vpc-lattice:us-west-2:000000000000:servicenetwork/sn-0123456789abcdef0",
                    "targetGroupArn":
                        "arn:aws:vpc-lattice:us-west-2:000000000000:targetgroup/tg-0123456789abcdef0",
                    "region": "us-west-2",
                    "timeEpoch": "1500",
                }),
            ),
        ] {
            let read = v2.get(member).cloned().unwrap_or(Value::Null);
            assert_eq!(
                read, expected,
                "case \"the V2 event\": expected {member} {expected}, got {read}"
            );
        }

        // Bytes that are no text travel base64-encoded, with the flag set.
        let binary = event(
            &target(crate::state::DEFAULT_LAMBDA_EVENT_STRUCTURE_VERSION, None),
            &request("PUT", "/", "", &[0xff, 0x00]),
            at,
        );
        assert_eq!(
            binary.get(V1_BODY).cloned().unwrap_or(Value::Null),
            Value::from(::aws_smithy_types::base64::encode([0xff_u8, 0x00])),
            "case \"a body that is no text\": expected the bytes base64-encoded, got {binary}"
        );
        assert_eq!(
            binary.get(V1_BASE64).cloned().unwrap_or(Value::Null),
            Value::from(true),
            "case \"a body that is no text\": expected the flag set, got {binary}"
        );

        // A service these records cannot place in ONE service network reports none.
        let unplaced = event(&target(V2_EVENT, None), &request("GET", "/", "", &[]), at);
        let context = unplaced
            .get(V2_REQUEST_CONTEXT)
            .cloned()
            .unwrap_or(Value::Null);
        assert!(
            context.get(V2_SERVICE_NETWORK_ARN).is_none(),
            "case \"a service of no single network\": expected no serviceNetworkArn, got \
             {context}"
        );
    }

    /// RULE: the client is answered from the object the function returned only while that
    /// object satisfies the response contract; every other payload is the service's own
    /// [`BAD_GATEWAY_STATUS`], and the payload is never passed through.
    ///
    /// The rows span what the rule generalises over: a well-formed answer (text body, no body,
    /// a base64-encoded body, a status other than 200, a status stated as text), and each class
    /// of violation (no status, a status outside the HTTP range, a payload that is no object, a
    /// headers member of the wrong shape, a body of the wrong shape, a flag of the wrong
    /// shape).
    #[test]
    fn a_response_is_mapped_only_when_it_satisfies_the_contract() {
        /// One row: the payload a function returned, and the status and body the client is
        /// answered with — `None` where the contract refuses the payload.
        struct Row {
            case: &'static str,
            payload: &'static str,
            answered: Option<(u16, &'static str)>,
        }
        let rows = [
            Row {
                case: "a status and a text body",
                payload: r#"{"statusCode": 200, "body": "SERVED"}"#,
                answered: Some((200, "SERVED")),
            },
            Row {
                case: "a status and no body",
                payload: r#"{"statusCode": 204}"#,
                answered: Some((204, "")),
            },
            Row {
                case: "a base64-encoded body",
                payload: r#"{"statusCode": 201, "body": "U0VSVkVE", "isBase64Encoded": true}"#,
                answered: Some((201, "SERVED")),
            },
            Row {
                case: "a status stated as text",
                payload: r#"{"statusCode": "404", "body": "gone"}"#,
                answered: Some((404, "gone")),
            },
            Row {
                case: "no status at all",
                payload: r#"{"body": "SERVED"}"#,
                answered: None,
            },
            Row {
                case: "a status outside the HTTP range",
                payload: r#"{"statusCode": 1200, "body": "SERVED"}"#,
                answered: None,
            },
            Row {
                case: "a payload that is no object",
                payload: r#""SERVED""#,
                answered: None,
            },
            Row {
                case: "a payload that is no JSON",
                payload: "not json at all",
                answered: None,
            },
            Row {
                case: "a headers member that is no object",
                payload: r#"{"statusCode": 200, "headers": ["content-type", "text/plain"]}"#,
                answered: None,
            },
            Row {
                case: "a body that is no text",
                payload: r#"{"statusCode": 200, "body": {"not": "text"}}"#,
                answered: None,
            },
            Row {
                case: "a base64 flag that is no boolean",
                payload: r#"{"statusCode": 200, "body": "SERVED", "isBase64Encoded": "yes"}"#,
                answered: None,
            },
        ];
        for row in rows {
            let case = row.case;
            let mapped = mapped_response(row.payload.as_bytes());
            match row.answered {
                Some((status, body)) => {
                    let served = mapped.as_ref().unwrap_or_else(|reason| {
                        panic!("case {case:?}: expected a mapped response, got {reason}")
                    });
                    assert_eq!(
                        (served.status, String::from_utf8_lossy(&served.body).to_string()),
                        (status, body.to_string()),
                        "case {case:?}: expected ({status}, {body:?}), got ({}, {:?})",
                        served.status,
                        String::from_utf8_lossy(&served.body)
                    );
                }
                None => assert!(
                    mapped.is_err(),
                    "case {case:?}: expected the contract to refuse the response, got {:?}",
                    mapped.map(|served| served.status)
                ),
            }
        }
    }

    /// RULE: the headers the returned object names travel with the answer, lower-cased.
    #[test]
    fn the_headers_the_response_names_travel_with_it() {
        let served = mapped_response(
            br#"{"statusCode": 200, "headers": {"Content-Type": "text/plain", "X-Count": 7},
                 "body": "SERVED"}"#,
        )
        .expect("the response satisfies the contract");
        let mut headers = served.headers.clone();
        headers.sort();
        assert_eq!(
            headers,
            vec![
                ("content-type".to_string(), "text/plain".to_string()),
                ("x-count".to_string(), "7".to_string()),
            ],
            "expected the named headers lower-cased, got {:?}",
            served.headers
        );
    }
}
