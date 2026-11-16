// SPDX-License-Identifier: CC0-1.0

//! The LAMBDA@EDGE function a cache behaviour associates, RUN on the viewer event it names.
//!
//! `LambdaFunctionAssociation` is "a complex type that contains a Lambda@Edge function
//! association", its `EventType` is "specifies the event type that triggers a Lambda@Edge
//! function invocation", and its `LambdaFunctionARN` is "the ARN of the Lambda@Edge function.
//! You must specify the ARN of a function version; you can't specify an alias or $LATEST". So
//! a behaviour that carries an association INVOKES that function version on the event it
//! names, and what the function RETURNS is what CloudFront continues with: the request it
//! then keys on and fetches, or the response it then serves the viewer without asking the
//! origin at all.
//!
//! The function is a LAMBDA function, so it is run by LAMBDA: the invocation is a
//! `RequestResponse` `Invoke` of the version the association names, made over the emulator's
//! own in-process transport (the operation callback, which opens no socket and reaches
//! nothing outside this process). Nothing here interprets the caller's code — the service
//! that owns the function runs it — so what a viewer is served at the edge and what an
//! `Invoke` of the same version answers cannot disagree.
//!
//! # The event object
//!
//! The function is handed the event object CloudFront passes a Lambda@Edge function: one
//! `Records` entry whose `cf` member carries the `config` of the request (the distribution,
//! the event type and the request's identifier) and the `request` — its `clientIp`, its
//! `method`, its `uri` (the viewer path WITHIN the distribution), its `querystring` and its
//! `headers`, each name carrying a list of entries that state the header's own `key` and
//! `value` — with the `response` beside it on a viewer-response event. The viewer's BODY
//! travels in the event only where the association asks for it
//! (`LambdaFunctionAssociation$IncludeBody`, "a flag that allows a Lambda@Edge function to
//! have read access to the body content"), as `base64` data under `body`, truncated at
//! [`MAX_EVENT_BODY_BYTES`] with `inputTruncated` saying so.
//!
//! # What comes back
//!
//! What the function returns is read as the object CloudFront reads: on a viewer-request event
//! an object carrying `status` is a RESPONSE CloudFront serves straight away, and any other
//! object is the REQUEST it continues with (its `uri`, `querystring` and `headers` replace the
//! viewer's, while the method and the headers CloudFront documents as read-only keep the
//! values the viewer request had); on a viewer-response event it is the response the viewer
//! receives, whose bytes stay the ones the edge already had — a viewer-response function
//! states headers and a status, not a body.
//!
//! # A function that fails
//!
//! A run Lambda reports as errored, one that answered no object CloudFront can read, and one
//! that did not answer within [`FUNCTION_TIMEOUT`], leave CloudFront with neither a request
//! nor a response to continue with, so the viewer is answered [`FAILED_STATUS`] and no origin
//! request is made for it.

use std::time::Duration;

use ::serde_json::{Map, Value as Json};
use aws_smithy_runtime_api::http::Response as RtResponse;
use smithy_server_runtime::OperationContext;

use super::functions::{
    kept_request_headers, kept_response_headers, status_reason, text, Event, Generated, Request,
    Rewritten, MAX_STATUS, MIN_STATUS, READ_ONLY_REQUEST_HEADERS, READ_ONLY_RESPONSE_HEADERS,
    VIEWER_REQUEST, VIEWER_RESPONSE,
};
use crate::ops::types::LambdaFunctionAssociations;

/// The status a viewer request gets when the Lambda@Edge function the matched behaviour
/// associates produced nothing for the edge to continue from.
///
/// CloudFront answers such a request `502`: the function was the thing that had to produce the
/// request or the response, and it produced neither, so there is no answer to serve and no
/// origin to ask for one.
pub const FAILED_STATUS: u16 = 502;

/// How long the edge waits for one Lambda@Edge invocation to answer.
///
/// An EMULATOR ceiling, deliberately GENEROUS: a viewer is held open for exactly as long as
/// the function takes, so a run that never answers would hold the viewer's connection for as
/// long as it lasted — but the run happens in this process, on a machine that may be busy with
/// everything else the developer is doing, so the bound is far past the time a function needs
/// rather than near it. A run slower than the host is quick is still the function's answer;
/// only one that never comes is not. Past it the edge has no object to continue from, which is
/// where a failed run leaves it too.
pub const FUNCTION_TIMEOUT: Duration = Duration::from_secs(60);

/// How many bytes of the viewer's BODY one event object carries, where the association asks
/// for the body at all.
///
/// An EMULATOR ceiling, for the reason [`FUNCTION_TIMEOUT`] is one: the body is the viewer's
/// to choose, and it is copied into the event this process hands Lambda. A body longer than
/// this travels truncated, which is exactly what the event object's own `inputTruncated`
/// member is for.
pub const MAX_EVENT_BODY_BYTES: usize = 40 * 1024;

/// The members of the Lambda@Edge event object this module writes and reads.
const RECORDS_MEMBER: &str = "Records";
const CF_MEMBER: &str = "cf";
const CONFIG_MEMBER: &str = "config";
const REQUEST_MEMBER: &str = "request";
const RESPONSE_MEMBER: &str = "response";
const DISTRIBUTION_DOMAIN_NAME_MEMBER: &str = "distributionDomainName";
const DISTRIBUTION_ID_MEMBER: &str = "distributionId";
const EVENT_TYPE_MEMBER: &str = "eventType";
const REQUEST_ID_MEMBER: &str = "requestId";
const CLIENT_IP_MEMBER: &str = "clientIp";
const METHOD_MEMBER: &str = "method";
const URI_MEMBER: &str = "uri";
const QUERYSTRING_MEMBER: &str = "querystring";
const HEADERS_MEMBER: &str = "headers";
const STATUS_MEMBER: &str = "status";
const STATUS_DESCRIPTION_MEMBER: &str = "statusDescription";
const BODY_MEMBER: &str = "body";
const BODY_ENCODING_MEMBER: &str = "bodyEncoding";
const KEY_MEMBER: &str = "key";
const VALUE_MEMBER: &str = "value";
const DATA_MEMBER: &str = "data";
const ENCODING_MEMBER: &str = "encoding";
const ACTION_MEMBER: &str = "action";
const INPUT_TRUNCATED_MEMBER: &str = "inputTruncated";

/// The `body.encoding` the event object states the viewer's bytes under, and the `action` it
/// states for a body a viewer-event function may read but not replace.
const ENCODING_BASE64: &str = "base64";
const ENCODING_TEXT: &str = "text";
const ACTION_READ_ONLY: &str = "read-only";

/// One Lambda@Edge association of a matched behaviour: WHICH function version runs, and
/// whether the event it is handed carries the viewer's body.
#[derive(Debug, Clone)]
pub struct Associated {
    /// `LambdaFunctionAssociation$LambdaFunctionARN` — the published function version this
    /// behaviour named, which is what the invocation addresses.
    pub arn: String,
    /// `LambdaFunctionAssociation$IncludeBody`, and `false` for an association that states
    /// none: "if you set it to true ... you can read the body".
    pub include_body: bool,
}

/// The Lambda@Edge functions one matched cache behaviour associates, by the event each one
/// runs on.
///
/// A behaviour carries at most one association per event type, so each event is one function's
/// (`LambdaFunctionAssociations` lists them and `EventType` names which event each runs on).
/// Only the VIEWER events are held here: the origin-facing ones are raised around the origin
/// fetch, which this plane decides separately.
#[derive(Debug, Clone, Default)]
pub struct Associations {
    /// The function that runs before CloudFront reads its cache, when the behaviour names one.
    pub viewer_request: Option<Associated>,
    /// The function that runs on the answer, when the behaviour names one.
    pub viewer_response: Option<Associated>,
}

impl Associations {
    /// Whether no Lambda@Edge function runs on a viewer event of this request at all, which is
    /// the common case and the one that costs the edge nothing.
    pub fn is_empty(&self) -> bool {
        self.viewer_request.is_none() && self.viewer_response.is_none()
    }
}

/// What the behaviour's `LambdaFunctionAssociations` resolve to.
///
/// The association names the function by ARN, and the function's code belongs to Lambda, so
/// nothing is read out of this service's store here: whether that version still exists is
/// answered by the invocation itself, the way it is for any caller of a function.
pub fn associations(associations: Option<&LambdaFunctionAssociations>) -> Associations {
    let mut resolved = Associations::default();
    for association in associations
        .and_then(|associations| associations.items.as_ref())
        .into_iter()
        .flatten()
    {
        let slot = match association.event_type.as_str() {
            VIEWER_REQUEST => &mut resolved.viewer_request,
            VIEWER_RESPONSE => &mut resolved.viewer_response,
            // An origin-facing event, and an event type no function runs on: neither names a
            // viewer event this module serves.
            _ => continue,
        };
        if slot.is_some() {
            continue;
        }
        *slot = Some(Associated {
            arn: association.lambda_function_arn.clone(),
            include_body: association.include_body.unwrap_or(false),
        });
    }
    resolved
}

/// Run the behaviour's viewer-REQUEST function over `event` and read what it returned.
///
/// `body` is the bytes the viewer sent, which travel in the event only where the association
/// asks for them.
pub async fn run_viewer_request(
    ctx: &OperationContext<'_>,
    associated: &Associated,
    event: &Event<'_>,
    body: &[u8],
) -> Rewritten {
    let object = event_object(event, VIEWER_REQUEST, Some((associated, body)), None);
    let Some(returned) = returned_object(ctx, associated, VIEWER_REQUEST, object).await else {
        return Rewritten::Failed;
    };
    // "If the function returns a response, it must contain `status`": which of the two a
    // function returned is decided by the object itself, so a viewer-request function that
    // built a response is read as having generated one.
    if returned.get(STATUS_MEMBER).is_some() {
        return match generated_response(&returned) {
            Some(generated) => Rewritten::Response(generated),
            None => Rewritten::Failed,
        };
    }
    if !returned.is_object() {
        return Rewritten::Failed;
    }
    Rewritten::Request(Request {
        uri: text(&returned, URI_MEMBER).unwrap_or_else(|| event.uri.to_string()),
        query: text(&returned, QUERYSTRING_MEMBER).unwrap_or_else(|| event.query.to_string()),
        headers: request_headers(&returned, &event.headers),
    })
}

/// Run the behaviour's viewer-RESPONSE function over `event` and the answer the edge is about
/// to serve, and hand back the answer the function returned.
///
/// `None` is a function that produced no response object to serve.
pub async fn run_viewer_response(
    ctx: &OperationContext<'_>,
    associated: &Associated,
    event: &Event<'_>,
    response: &RtResponse,
) -> Option<RtResponse> {
    let object = event_object(
        event,
        VIEWER_RESPONSE,
        None,
        Some(response_object(response)),
    );
    let returned = returned_object(ctx, associated, VIEWER_RESPONSE, object).await?;
    // A viewer-response function returns the RESPONSE: an object with no status is no response
    // at all, and leaves nothing to serve.
    let status = status_of(&returned)?;
    let mut served = RtResponse::new(
        ::aws_smithy_runtime_api::http::StatusCode::try_from(status).ok()?,
        ::aws_smithy_types::body::SdkBody::from(
            response
                .body()
                .bytes()
                .map(<[u8]>::to_vec)
                .unwrap_or_default(),
        ),
    );
    for (name, value) in response_headers(&returned, response) {
        let _ = served.headers_mut().try_append(name, value);
    }
    Some(served)
}

/// The event object one Lambda@Edge function is handed, as the JSON text the invocation
/// carries: one `Records` entry under `cf`, the way CloudFront passes it.
fn event_object(
    event: &Event<'_>,
    event_type: &str,
    body: Option<(&Associated, &[u8])>,
    response: Option<Json>,
) -> String {
    let mut config = Map::new();
    config.insert(
        DISTRIBUTION_DOMAIN_NAME_MEMBER.to_string(),
        Json::String(event.distribution_domain_name.to_string()),
    );
    config.insert(
        DISTRIBUTION_ID_MEMBER.to_string(),
        Json::String(event.distribution_id.to_string()),
    );
    config.insert(
        EVENT_TYPE_MEMBER.to_string(),
        Json::String(event_type.to_string()),
    );
    config.insert(
        REQUEST_ID_MEMBER.to_string(),
        Json::String(event.request_id.to_string()),
    );
    let mut request = Map::new();
    request.insert(
        CLIENT_IP_MEMBER.to_string(),
        Json::String(event.viewer_address.to_string()),
    );
    request.insert(
        METHOD_MEMBER.to_string(),
        Json::String(event.method.to_string()),
    );
    request.insert(URI_MEMBER.to_string(), Json::String(event.uri.to_string()));
    request.insert(
        QUERYSTRING_MEMBER.to_string(),
        Json::String(event.query.to_string()),
    );
    request.insert(
        HEADERS_MEMBER.to_string(),
        header_entries(event.headers.iter().cloned()),
    );
    // The body travels only where the association asks for it, and a request that carries none
    // has no body member to state.
    if let Some((associated, bytes)) = body {
        if associated.include_body && !bytes.is_empty() {
            request.insert(BODY_MEMBER.to_string(), body_object(bytes));
        }
    }
    let mut cf = Map::new();
    cf.insert(CONFIG_MEMBER.to_string(), Json::Object(config));
    cf.insert(REQUEST_MEMBER.to_string(), Json::Object(request));
    if let Some(response) = response {
        cf.insert(RESPONSE_MEMBER.to_string(), response);
    }
    let mut record = Map::new();
    record.insert(CF_MEMBER.to_string(), Json::Object(cf));
    let mut object = Map::new();
    object.insert(
        RECORDS_MEMBER.to_string(),
        Json::Array(vec![Json::Object(record)]),
    );
    Json::Object(object).to_string()
}

/// The `body` member of the event object for the viewer's bytes: the data as `base64`, read-only
/// on a viewer event, and `inputTruncated` for a body longer than one event carries.
fn body_object(bytes: &[u8]) -> Json {
    let truncated = bytes.len() > MAX_EVENT_BODY_BYTES;
    let carried = &bytes[..bytes.len().min(MAX_EVENT_BODY_BYTES)];
    let mut body = Map::new();
    body.insert(
        INPUT_TRUNCATED_MEMBER.to_string(),
        Json::Bool(truncated),
    );
    body.insert(
        ACTION_MEMBER.to_string(),
        Json::String(ACTION_READ_ONLY.to_string()),
    );
    body.insert(
        ENCODING_MEMBER.to_string(),
        Json::String(ENCODING_BASE64.to_string()),
    );
    body.insert(
        DATA_MEMBER.to_string(),
        Json::String(::base64::Engine::encode(
            &::base64::engine::general_purpose::STANDARD,
            carried,
        )),
    );
    Json::Object(body)
}

/// The response object of the answer the edge is about to serve, as the event object states
/// it: the status as the TEXT a Lambda@Edge object carries it in, its reason phrase, and the
/// headers the edge has for it.
fn response_object(response: &RtResponse) -> Json {
    let status = response.status().as_u16();
    let mut object = Map::new();
    object.insert(STATUS_MEMBER.to_string(), Json::String(status.to_string()));
    if let Some(reason) = status_reason(status) {
        object.insert(
            STATUS_DESCRIPTION_MEMBER.to_string(),
            Json::String(reason.to_string()),
        );
    }
    object.insert(
        HEADERS_MEMBER.to_string(),
        header_entries(
            response
                .headers()
                .iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), value.to_string())),
        ),
    );
    Json::Object(object)
}

/// One name's values as a Lambda@Edge event object states them: the lowercased name carries a
/// list of entries, each stating the header's own `key` and its `value`.
fn header_entries(pairs: impl Iterator<Item = (String, String)>) -> Json {
    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    for (name, value) in pairs {
        let name = name.to_ascii_lowercase();
        match grouped.iter_mut().find(|(held, _)| *held == name) {
            Some((_, values)) => values.push(value),
            None => grouped.push((name, vec![value])),
        }
    }
    let mut object = Map::new();
    for (name, values) in grouped {
        let entries: Vec<Json> = values
            .into_iter()
            .map(|value| {
                let mut entry = Map::new();
                entry.insert(KEY_MEMBER.to_string(), Json::String(name.clone()));
                entry.insert(VALUE_MEMBER.to_string(), Json::String(value));
                Json::Object(entry)
            })
            .collect();
        object.insert(name, Json::Array(entries));
    }
    Json::Object(object)
}

/// The headers one returned object states, flattened back into the `(name, value)` pairs an
/// HTTP message carries: each name's entries in the order the object listed them, under the
/// `key` the object stated for them where it stated one, and never one of `read_only`.
fn stated_headers(headers: Option<&Json>, read_only: &[&str]) -> Vec<(String, String)> {
    let Some(Json::Object(named)) = headers else {
        return Vec::new();
    };
    let mut flattened: Vec<(String, String)> = Vec::new();
    for (name, entries) in named {
        if read_only
            .iter()
            .any(|held| held.eq_ignore_ascii_case(name))
        {
            continue;
        }
        let entries = match entries {
            Json::Array(entries) => entries.clone(),
            // A name whose value the code stated as one entry rather than a list of them.
            entry @ Json::Object(_) => vec![entry.clone()],
            _ => continue,
        };
        for entry in entries {
            let Some(value) = text(&entry, VALUE_MEMBER) else {
                continue;
            };
            let stated = text(&entry, KEY_MEMBER).unwrap_or_else(|| name.clone());
            flattened.push((stated, value));
        }
    }
    flattened
}

/// The request's headers as the edge continues with them: what the function returned, with the
/// read-only ones kept as the viewer request had them — which names those are, and the values
/// kept for them, are the sibling plane's one reading of the same rule.
fn request_headers(returned: &Json, viewer: &[(String, String)]) -> Vec<(String, String)> {
    let mut headers = stated_headers(returned.get(HEADERS_MEMBER), &READ_ONLY_REQUEST_HEADERS);
    headers.extend(kept_request_headers(viewer));
    headers
}

/// The answer's headers as the viewer receives them: what the function returned, with the
/// read-only ones kept as the edge had them.
fn response_headers(returned: &Json, response: &RtResponse) -> Vec<(String, String)> {
    let mut headers = stated_headers(returned.get(HEADERS_MEMBER), &READ_ONLY_RESPONSE_HEADERS);
    headers.extend(kept_response_headers(response));
    headers
}

/// The response a function generated itself: its status, its headers and the bytes it stated.
fn generated_response(returned: &Json) -> Option<Generated> {
    let status = status_of(returned)?;
    Some(Generated {
        status,
        headers: stated_headers(returned.get(HEADERS_MEMBER), &[]),
        body: body_bytes(returned),
    })
}

/// The `status` a returned object carries, as a status a viewer can be answered with.
///
/// "The `status` value must be a string": the member is the status in text, and the same status
/// stated as a JSON number names the same status.
fn status_of(returned: &Json) -> Option<u16> {
    let status = match returned.get(STATUS_MEMBER)? {
        Json::String(text) => text.trim().parse().ok()?,
        Json::Number(number) => number.as_u64()?,
        _ => return None,
    };
    (MIN_STATUS..=MAX_STATUS)
        .contains(&status)
        .then(|| u16::try_from(status).ok())
        .flatten()
}

/// The bytes a generated response states: `body` as it stands, or the data its `base64`
/// `bodyEncoding` decodes to.
fn body_bytes(returned: &Json) -> Vec<u8> {
    let Some(data) = text(returned, BODY_MEMBER) else {
        return Vec::new();
    };
    match text(returned, BODY_ENCODING_MEMBER).as_deref() {
        Some(ENCODING_BASE64) => {
            ::base64::Engine::decode(&::base64::engine::general_purpose::STANDARD, &data)
                .unwrap_or_default()
        }
        // "text" and a response stating no encoding, which is the same thing — "the default
        // value is text" — the data as it stands.
        Some(ENCODING_TEXT) | None => data.into_bytes(),
        // An encoding this event object does not define decodes to nothing.
        Some(_) => Vec::new(),
    }
}

/// Invoke the function version `associated` names with `object` and read the object it
/// returned, or `None` when the edge learned nothing it can continue from.
///
/// The invocation is `RequestResponse` — the edge has a viewer waiting on what the function
/// returns — and it is addressed to the Region the association's own ARN names, which is the
/// Region the function lives in. No store lock is held across it.
async fn returned_object(
    ctx: &OperationContext<'_>,
    associated: &Associated,
    event_type: &str,
    object: String,
) -> Option<Json> {
    let Some(region) = crate::state::lambda_region_of(&associated.arn) else {
        ::tracing::warn!(
            function = %associated.arn,
            event_type = %event_type,
            "the Lambda@Edge association this cache behaviour carries names no Region, so no \
             function was invoked for this request"
        );
        return None;
    };
    let client = crate::state::lambda_client(ctx, region);
    let invocation = client
        .invoke()
        .function_name(&associated.arn)
        .invocation_type(::aws_sdk_lambda::types::InvocationType::RequestResponse)
        .payload(::aws_sdk_lambda::primitives::Blob::new(object.into_bytes()))
        .send();
    let answered = match ::tokio::time::timeout(FUNCTION_TIMEOUT, invocation).await {
        Ok(Ok(answered)) => answered,
        Ok(Err(error)) => {
            ::tracing::warn!(
                function = %associated.arn,
                event_type = %event_type,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the Lambda@Edge function this cache behaviour associates was not invoked, so \
                 the edge has nothing to continue from"
            );
            return None;
        }
        Err(_elapsed) => {
            ::tracing::warn!(
                function = %associated.arn,
                event_type = %event_type,
                waited_ms = FUNCTION_TIMEOUT.as_millis(),
                "the Lambda@Edge function this cache behaviour associates did not answer in \
                 time, so the edge has nothing to continue from"
            );
            return None;
        }
    };
    // "FunctionError: if present, indicates that an error occurred during function
    // execution" — a run that errored returned no event object.
    if let Some(signal) = answered.function_error() {
        ::tracing::warn!(
            function = %associated.arn,
            event_type = %event_type,
            function_error = %signal,
            "the Lambda@Edge function this cache behaviour associates errored, so the edge has \
             nothing to continue from"
        );
        return None;
    }
    let returned = answered
        .payload()
        .map(|payload| payload.as_ref().to_vec())
        .unwrap_or_default();
    match ::serde_json::from_slice::<Json>(&returned) {
        Ok(returned) => Some(returned),
        Err(error) => {
            ::tracing::warn!(
                function = %associated.arn,
                event_type = %event_type,
                %error,
                "the object the Lambda@Edge function returned could not be read, so the edge \
                 has nothing to continue from"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::LambdaFunctionAssociation;

    /// The version ARN of `name`, as an association names a function.
    fn version(name: &str) -> String {
        format!("arn:aws:lambda:us-east-1:000000000000:function:{name}:1")
    }

    /// `stated` takes the same rows the resolution table writes.
    fn stated(items: &[Stated<'_>]) -> LambdaFunctionAssociations {
        LambdaFunctionAssociations {
            quantity: i32::try_from(items.len()).unwrap_or(i32::MAX),
            items: Some(
                items
                    .iter()
                    .map(|(arn, event_type, include_body)| LambdaFunctionAssociation {
                        lambda_function_arn: (*arn).to_string(),
                        event_type: (*event_type).to_string(),
                        include_body: *include_body,
                    })
                    .collect(),
            ),
        }
    }

    /// One stated association as a row writes it: the ARN it names, the event type it fires
    /// on, and what it says about the body.
    type Stated<'a> = (&'a str, &'a str, Option<bool>);

    /// One row of the resolution table: the associations a behaviour states, and the function
    /// each viewer event must then resolve to.
    struct Resolution<'a> {
        case: &'static str,
        items: Vec<Stated<'a>>,
        request: Option<&'a str>,
        response: Option<&'a str>,
    }

    /// RULE: each VIEWER event of a behaviour resolves to the function the association for
    /// that event names, and nothing else does.
    #[test]
    fn the_viewer_events_resolve_to_the_functions_their_associations_name() {
        let request = version("edge-request");
        let response = version("edge-response");
        // The rows differ along what the resolution generalises over: which events the
        // behaviour associates, the order it states them in, a repeated event, an
        // origin-facing event and an event type no function runs on.
        let rows: Vec<Resolution<'_>> = vec![
            Resolution {
                case: "no associations at all",
                items: vec![],
                request: None,
                response: None,
            },
            Resolution {
                case: "the viewer-request event alone",
                items: vec![(request.as_str(), VIEWER_REQUEST, Some(true))],
                request: Some(request.as_str()),
                response: None,
            },
            Resolution {
                case: "both viewer events, response first",
                items: vec![
                    (response.as_str(), VIEWER_RESPONSE, None),
                    (request.as_str(), VIEWER_REQUEST, None),
                ],
                request: Some(request.as_str()),
                response: Some(response.as_str()),
            },
            Resolution {
                case: "the viewer-request event stated twice",
                items: vec![
                    (request.as_str(), VIEWER_REQUEST, None),
                    (response.as_str(), VIEWER_REQUEST, None),
                ],
                request: Some(request.as_str()),
                response: None,
            },
            Resolution {
                case: "an origin-facing event",
                items: vec![(request.as_str(), "origin-request", None)],
                request: None,
                response: None,
            },
            Resolution {
                case: "an event type no function runs on",
                items: vec![(request.as_str(), "viewer-redirect", None)],
                request: None,
                response: None,
            },
        ];
        for row in rows {
            let case = row.case;
            let stated = stated(&row.items);
            let resolved = associations(Some(&stated));
            let got_request = resolved.viewer_request.as_ref().map(|held| held.arn.clone());
            let got_response = resolved
                .viewer_response
                .as_ref()
                .map(|held| held.arn.clone());
            assert_eq!(
                got_request.as_deref(),
                row.request,
                "case {case:?}: expected {:?} on the viewer-request event, got {got_request:?}",
                row.request
            );
            assert_eq!(
                got_response.as_deref(),
                row.response,
                "case {case:?}: expected {:?} on the viewer-response event, got {got_response:?}",
                row.response
            );
            assert_eq!(
                resolved.is_empty(),
                row.request.is_none() && row.response.is_none(),
                "case {case:?}: the resolution reports {:?} for a behaviour whose viewer events \
                 are {got_request:?} and {got_response:?}",
                resolved.is_empty()
            );
        }
        // A behaviour that states a quantity and no items associates nothing.
        let empty = associations(None);
        assert!(
            empty.is_empty(),
            "case \"no LambdaFunctionAssociations member\": expected no association, got {empty:?}"
        );
    }

    /// RULE: `IncludeBody` is what decides whether the event carries the viewer's body, and
    /// an association that states none carries none.
    #[test]
    fn the_body_travels_only_where_the_association_asks_for_it() {
        let arn = version("edge-body");
        // A body LONGER than one event carries, so the row below reads what truncation did
        // rather than the whole body again: one byte per index, so the carried prefix is
        // recognisable.
        let oversized: Vec<u8> = (0..MAX_EVENT_BODY_BYTES + 64)
            .map(|index| u8::try_from(index % 251).unwrap_or_default())
            .collect();
        // The rows differ along the flag, along whether the request has a body at all, and
        // along whether that body is longer than one event carries.
        let rows: [(&str, Option<bool>, &[u8], bool); 5] = [
            ("include_body true with bytes", Some(true), b"hello", true),
            ("include_body true with no bytes", Some(true), b"", false),
            ("include_body false with bytes", Some(false), b"hello", false),
            ("include_body unstated with bytes", None, b"hello", false),
            (
                "include_body true with a longer body than one event carries",
                Some(true),
                oversized.as_slice(),
                true,
            ),
        ];
        for (case, include_body, body, carried) in rows {
            let stated = stated(&[(arn.as_str(), VIEWER_REQUEST, include_body)]);
            let resolved = associations(Some(&stated));
            let associated = resolved
                .viewer_request
                .unwrap_or_else(|| panic!("case {case:?}: the viewer-request event resolves"));
            let event = Event {
                distribution_id: "E1EXAMPLE",
                distribution_domain_name: "example.cloudfront.net",
                request_id: "cf-id",
                viewer_address: "203.0.113.178",
                method: "POST",
                uri: "/sim2real/first",
                query: "",
                headers: vec![("host".to_string(), "example.cloudfront.net".to_string())],
            };
            let object = event_object(&event, VIEWER_REQUEST, Some((&associated, body)), None);
            let parsed: Json =
                ::serde_json::from_str(&object).expect("the event object is written as JSON");
            let request = &parsed[RECORDS_MEMBER][0][CF_MEMBER][REQUEST_MEMBER];
            assert_eq!(
                request.get(BODY_MEMBER).is_some(),
                carried,
                "case {case:?}: expected the body carried={carried}, got {request}"
            );
            assert_eq!(
                request[URI_MEMBER],
                Json::String(event.uri.to_string()),
                "case {case:?}: the event states the viewer path, got {request}"
            );
            assert_eq!(
                parsed[RECORDS_MEMBER][0][CF_MEMBER][CONFIG_MEMBER][EVENT_TYPE_MEMBER],
                Json::String(VIEWER_REQUEST.to_string()),
                "case {case:?}: the event states its own type, got {parsed}"
            );
            if carried {
                let data = request[BODY_MEMBER][DATA_MEMBER]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let decoded = ::base64::Engine::decode(
                    &::base64::engine::general_purpose::STANDARD,
                    &data,
                )
                .unwrap_or_default();
                let expected = &body[..body.len().min(MAX_EVENT_BODY_BYTES)];
                assert_eq!(
                    decoded.len(),
                    expected.len(),
                    "case {case:?}: expected {} byte(s) of the viewer's body in the event, got \
                     {}",
                    expected.len(),
                    decoded.len()
                );
                assert_eq!(
                    decoded,
                    expected.to_vec(),
                    "case {case:?}: the event carries the viewer's own bytes, got {} byte(s)",
                    decoded.len()
                );
                // `inputTruncated` is what says whether the function is seeing the whole body.
                let truncated = body.len() > MAX_EVENT_BODY_BYTES;
                assert_eq!(
                    request[BODY_MEMBER][INPUT_TRUNCATED_MEMBER],
                    Json::Bool(truncated),
                    "case {case:?}: expected inputTruncated={truncated} for a body of {} \
                     byte(s), got {}",
                    body.len(),
                    request[BODY_MEMBER]
                );
            }
        }
    }

    /// RULE: the status a returned object carries is read whichever of the two ways JSON
    /// states it, and only a status a viewer can be answered with is one.
    #[test]
    fn a_returned_response_is_read_from_the_status_it_states() {
        // The rows differ along the form of the value and along whether it names an HTTP
        // status at all.
        let rows: [(&str, Json, Option<u16>); 8] = [
            ("the text form", ::serde_json::json!({"status": "200"}), Some(200)),
            ("the number form", ::serde_json::json!({"status": 404}), Some(404)),
            ("the smallest status", ::serde_json::json!({"status": "100"}), Some(100)),
            ("the largest status", ::serde_json::json!({"status": "599"}), Some(599)),
            ("one below the smallest", ::serde_json::json!({"status": "99"}), None),
            ("one above the largest", ::serde_json::json!({"status": "600"}), None),
            ("no status at all", ::serde_json::json!({"uri": "/elsewhere"}), None),
            ("a status that is no number", ::serde_json::json!({"status": "ok"}), None),
        ];
        for (case, returned, expected) in rows {
            let read = status_of(&returned);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?} for {returned}"
            );
        }
    }

    /// One row of the generated-response table: the object a function returned, and the bytes
    /// and headers the edge must then serve.
    struct GeneratedRow {
        case: &'static str,
        returned: Json,
        body: Vec<u8>,
        headers: Vec<(&'static str, &'static str)>,
    }

    /// RULE: a generated response carries the headers the object stated and the bytes its
    /// `bodyEncoding` decodes to.
    #[test]
    fn a_generated_response_carries_the_headers_and_bytes_it_states() {
        // The rows differ along the encoding the object states and along the header forms it
        // uses: the entry list, a single entry, and a name whose `key` states its own case.
        let rows: Vec<GeneratedRow> = vec![
            GeneratedRow {
                case: "text by default",
                returned: ::serde_json::json!({
                    "status": "200",
                    "headers": {"x-marker": [{"key": "X-Marker", "value": "one"}]},
                    "body": "MARK /first",
                }),
                body: b"MARK /first".to_vec(),
                headers: vec![("X-Marker", "one")],
            },
            GeneratedRow {
                case: "text stated",
                returned: ::serde_json::json!({
                    "status": "200",
                    "bodyEncoding": "text",
                    "headers": {"x-marker": {"value": "one"}},
                    "body": "MARK /second",
                }),
                body: b"MARK /second".to_vec(),
                headers: vec![("x-marker", "one")],
            },
            GeneratedRow {
                case: "base64 stated",
                returned: ::serde_json::json!({
                    "status": "201",
                    "bodyEncoding": "base64",
                    "headers": {},
                    "body": "TUFSSw==",
                }),
                body: b"MARK".to_vec(),
                headers: vec![],
            },
            GeneratedRow {
                case: "a name carrying two values",
                returned: ::serde_json::json!({
                    "status": "200",
                    "headers": {"x-marker": [
                        {"key": "X-Marker", "value": "one"},
                        {"key": "X-Marker", "value": "two"},
                    ]},
                }),
                body: Vec::new(),
                headers: vec![("X-Marker", "one"), ("X-Marker", "two")],
            },
        ];
        for row in rows {
            let case = row.case;
            let generated = generated_response(&row.returned).unwrap_or_else(|| {
                panic!("case {case:?}: {} is a generated response", row.returned)
            });
            assert_eq!(
                generated.body, row.body,
                "case {case:?}: expected the bytes {:?}, got {:?}",
                row.body, generated.body
            );
            let stated: Vec<(String, String)> = row
                .headers
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect();
            assert_eq!(
                generated.headers, stated,
                "case {case:?}: expected the headers {stated:?}, got {:?}",
                generated.headers
            );
        }
    }
}
