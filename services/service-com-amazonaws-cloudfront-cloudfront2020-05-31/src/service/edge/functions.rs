// SPDX-License-Identifier: CC0-1.0

//! The CLOUDFRONT FUNCTION a cache behaviour associates, RUN on the viewer event it names.
//!
//! `FunctionAssociation` is "a CloudFront function that is associated with a cache behavior in
//! a CloudFront distribution", and its `EventType` is "the event type of the function, either
//! `viewer-request` or `viewer-response`". So a behaviour that carries an association runs
//! that function's code on the event it names, and what the function RETURNS is what
//! CloudFront continues with: the request it then keys on and fetches, or the response it then
//! serves the viewer.
//!
//! The code runs through the ONE engine this service executes a CloudFront function in
//! ([`crate::state::run_function`]) — the same engine `TestFunction` answers from, under the
//! same bounds — so what a function does at the edge and what a test of it reports cannot
//! disagree. The code is the `LIVE` stage's: "your functions must be published to the `LIVE`
//! stage to associate them with a cache behavior".
//!
//! # The event object
//!
//! The function is handed the event object CloudFront passes it, version `1.0`: the `context`
//! of the request (the distribution, the event type and the request's identifier), the
//! `viewer` it came from, the `request` — its method, its URI, its query string, its headers
//! and its cookies, each value under its own `value` member and with `multiValue` when a name
//! carries more than one — and, on a viewer-response event, the `response` beside it. Cookies
//! travel in `cookies` rather than in `headers`, which is where the event object carries them.
//!
//! # What a function may change
//!
//! What comes back is read as the event object's own members: on a viewer-request event the
//! returned object is a REQUEST (its URI, query string, headers and cookies replace the
//! viewer's) unless it carries `statusCode`, which makes it a RESPONSE CloudFront serves
//! straight away without asking the origin; on a viewer-response event it is the response the
//! viewer receives. A header CloudFront documents as read-only for the event keeps the value
//! the edge had for it, whatever the function returned, and the response's bytes are the ones
//! the edge already had — a viewer-response function states headers and a status, not a body.
//!
//! # A function that fails
//!
//! A run that raised, that did not compile, that returned something which is no event object,
//! or that a bound of the runtime stopped, leaves CloudFront with no request and no response
//! to continue with, so the viewer is answered `503` with CloudFront's own page
//! ([`FAILED_STATUS`]) and no origin request is made for it.

use ::serde_json::{Map, Value as Json};
use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};

use super::forwarding;
use crate::ops::types::FunctionAssociations;
use crate::state::{
    run_function, ComAmazonawsCloudfrontCloudfront20200531State, FunctionOutcome,
    FUNCTION_STAGE_LIVE,
};

/// `FunctionAssociation$EventType` for the event that runs before CloudFront looks in its
/// cache: "`viewer-request`".
pub const VIEWER_REQUEST: &str = "viewer-request";

/// `FunctionAssociation$EventType` for the event that runs on the way back to the viewer:
/// "`viewer-response`".
pub const VIEWER_RESPONSE: &str = "viewer-response";

/// The version of the event object CloudFront passes a function, which the object states
/// itself.
const EVENT_VERSION: &str = "1.0";

/// The status the viewer is answered with when the associated function did not produce an
/// event object to continue from.
pub const FAILED_STATUS: u16 = 503;

/// The members of the event object this module reads and writes.
const VERSION_MEMBER: &str = "version";
const CONTEXT_MEMBER: &str = "context";
const VIEWER_MEMBER: &str = "viewer";
const REQUEST_MEMBER: &str = "request";
const RESPONSE_MEMBER: &str = "response";
const DISTRIBUTION_DOMAIN_NAME_MEMBER: &str = "distributionDomainName";
const DISTRIBUTION_ID_MEMBER: &str = "distributionId";
const EVENT_TYPE_MEMBER: &str = "eventType";
const REQUEST_ID_MEMBER: &str = "requestId";
const IP_MEMBER: &str = "ip";
const METHOD_MEMBER: &str = "method";
const URI_MEMBER: &str = "uri";
const QUERYSTRING_MEMBER: &str = "querystring";
const HEADERS_MEMBER: &str = "headers";
const COOKIES_MEMBER: &str = "cookies";
const STATUS_CODE_MEMBER: &str = "statusCode";
const STATUS_DESCRIPTION_MEMBER: &str = "statusDescription";
const BODY_MEMBER: &str = "body";
const VALUE_MEMBER: &str = "value";
const MULTI_VALUE_MEMBER: &str = "multiValue";
const ATTRIBUTES_MEMBER: &str = "attributes";
const ENCODING_MEMBER: &str = "encoding";
const DATA_MEMBER: &str = "data";

/// The `body.encoding` a generated response states its bytes under: "`text`" for the data as
/// it stands, "`base64`" for data to decode first.
const ENCODING_TEXT: &str = "text";
const ENCODING_BASE64: &str = "base64";

/// The request header the viewer's cookies arrive in, which the event object carries in its
/// `cookies` member instead.
const COOKIE_HEADER: &str = "cookie";

/// The response header a cookie is set in, which the event object's response carries in its
/// `cookies` member instead.
const SET_COOKIE_HEADER: &str = "set-cookie";

/// The headers a function may NOT change on a viewer REQUEST event, so the value the viewer
/// request already had is kept whatever the function returned for it.
const READ_ONLY_REQUEST_HEADERS: [&str; 4] = ["content-length", "host", "transfer-encoding", "via"];

/// The headers a function may NOT change on a viewer RESPONSE event. The two that describe
/// the bytes the edge is serving are here because the edge — not the function — decided them:
/// an answer may have been compressed for this viewer before the event ran
/// ([`super::compression`]), so a function's value for them would describe bytes nobody sent.
const READ_ONLY_RESPONSE_HEADERS: [&str; 4] = [
    "content-encoding",
    "content-length",
    "transfer-encoding",
    "via",
];

/// The smallest and largest status a response object may carry: an HTTP status code names a
/// class between these, so a value outside them is no status a viewer can be answered with.
const MIN_STATUS: u64 = 100;
const MAX_STATUS: u64 = 599;

/// The code of one function association: which function, and the bytes of the `LIVE` stage
/// that run.
#[derive(Debug, Clone)]
pub struct Associated {
    /// `FunctionAssociation$FunctionARN` — the function this behaviour named.
    pub arn: String,
    /// The `LIVE` stage's code, as the store holds it.
    pub code: Vec<u8>,
}

/// The functions one matched cache behaviour associates, by the event each one runs on.
///
/// A behaviour carries at most one association per event type, so each event is one function's
/// (`FunctionAssociations` lists them and `EventType` names which event each runs on).
#[derive(Debug, Clone, Default)]
pub struct Associations {
    /// The function that runs before CloudFront reads its cache, when the behaviour names one.
    pub viewer_request: Option<Associated>,
    /// The function that runs on the answer, when the behaviour names one.
    pub viewer_response: Option<Associated>,
}

impl Associations {
    /// Whether no function runs for this request at all, which is the common case and the one
    /// that costs the edge nothing.
    pub fn is_empty(&self) -> bool {
        self.viewer_request.is_none() && self.viewer_response.is_none()
    }
}

/// What the behaviour's `FunctionAssociations` resolve to, read under the store's lock.
///
/// An association naming a function this account no longer holds, or one whose `LIVE` stage
/// is gone, resolves to NO code: such a configuration cannot be created
/// (`function_association_without_live_stage` refuses it), so this is a configuration that
/// lost its function, and a request under it is served the way one under a behaviour with no
/// association is.
pub fn associations(
    store: &ComAmazonawsCloudfrontCloudfront20200531State,
    associations: Option<&FunctionAssociations>,
) -> Associations {
    let mut resolved = Associations::default();
    for association in associations
        .and_then(|associations| associations.items.as_ref())
        .into_iter()
        .flatten()
    {
        let slot = match association.event_type.as_str() {
            VIEWER_REQUEST => &mut resolved.viewer_request,
            VIEWER_RESPONSE => &mut resolved.viewer_response,
            // An event type no CloudFront function runs on cannot be stored, and names no
            // event this module serves.
            _ => continue,
        };
        if slot.is_some() {
            continue;
        }
        let live = store
            .function_by_arn(&association.function_arn)
            .and_then(|function| function.stage(FUNCTION_STAGE_LIVE));
        if let Some(live) = live {
            *slot = Some(Associated {
                arn: association.function_arn.clone(),
                code: live.code.clone(),
            });
        } else {
            ::tracing::warn!(
                function = %association.function_arn,
                event_type = %association.event_type,
                "this cache behaviour's function association names no code published to the \
                 LIVE stage, so no function runs for this request"
            );
        }
    }
    resolved
}

/// Everything the event object says about the request this function is handed, read once by
/// the caller so the request a function sees is the request the edge resolved.
#[derive(Debug)]
pub struct Event<'r> {
    /// `context.distributionId`.
    pub distribution_id: &'r str,
    /// `context.distributionDomainName` — the domain name CloudFront assigned the
    /// distribution.
    pub distribution_domain_name: &'r str,
    /// `context.requestId` — the identifier of the request the edge is serving.
    pub request_id: &'r str,
    /// `viewer.ip`.
    pub viewer_address: &'r str,
    /// `request.method`.
    pub method: &'r str,
    /// `request.uri` — the viewer path WITHIN the distribution.
    pub uri: &'r str,
    /// `request.querystring`, as the viewer sent it.
    pub query: &'r str,
    /// `request.headers` and `request.cookies`, read off the viewer request.
    pub headers: Vec<(String, String)>,
}

impl<'r> Event<'r> {
    /// The request half of the event object for `req`, as the edge resolved it.
    pub fn of_request(
        distribution_id: &'r str,
        distribution_domain_name: &'r str,
        request_id: &'r str,
        viewer_address: &'r str,
        req: &'r RtRequest,
        uri: &'r str,
        query: &'r str,
    ) -> Self {
        Self {
            distribution_id,
            distribution_domain_name,
            request_id,
            viewer_address,
            method: req.method(),
            uri,
            query,
            headers: req
                .headers()
                .iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), value.to_string()))
                .collect(),
        }
    }

    /// The event object for one event type, as the JSON text the function is called with.
    fn object(&self, event_type: &str, response: Option<Json>) -> String {
        let mut context = Map::new();
        context.insert(
            DISTRIBUTION_DOMAIN_NAME_MEMBER.to_string(),
            Json::String(self.distribution_domain_name.to_string()),
        );
        context.insert(
            DISTRIBUTION_ID_MEMBER.to_string(),
            Json::String(self.distribution_id.to_string()),
        );
        context.insert(
            EVENT_TYPE_MEMBER.to_string(),
            Json::String(event_type.to_string()),
        );
        context.insert(
            REQUEST_ID_MEMBER.to_string(),
            Json::String(self.request_id.to_string()),
        );
        let mut viewer = Map::new();
        viewer.insert(
            IP_MEMBER.to_string(),
            Json::String(self.viewer_address.to_string()),
        );
        let mut request = Map::new();
        request.insert(
            METHOD_MEMBER.to_string(),
            Json::String(self.method.to_string()),
        );
        request.insert(URI_MEMBER.to_string(), Json::String(self.uri.to_string()));
        request.insert(
            QUERYSTRING_MEMBER.to_string(),
            named_values(query_pairs(self.query)),
        );
        request.insert(
            HEADERS_MEMBER.to_string(),
            named_values(
                self.headers
                    .iter()
                    .filter(|(name, _)| name != COOKIE_HEADER)
                    .map(|(name, value)| (name.clone(), value.clone())),
            ),
        );
        request.insert(COOKIES_MEMBER.to_string(), cookies_of(&self.headers));
        let mut event = Map::new();
        event.insert(
            VERSION_MEMBER.to_string(),
            Json::String(EVENT_VERSION.to_string()),
        );
        event.insert(CONTEXT_MEMBER.to_string(), Json::Object(context));
        event.insert(VIEWER_MEMBER.to_string(), Json::Object(viewer));
        event.insert(REQUEST_MEMBER.to_string(), Json::Object(request));
        if let Some(response) = response {
            event.insert(RESPONSE_MEMBER.to_string(), response);
        }
        Json::Object(event).to_string()
    }
}

/// What a viewer-REQUEST function came back with.
#[derive(Debug)]
pub enum Rewritten {
    /// The function returned a REQUEST: what the edge keys on and fetches from here on.
    Request(Request),
    /// The function returned a RESPONSE: the answer CloudFront serves straight away, having
    /// made no origin request at all.
    Response(Generated),
    /// The function produced no event object, so there is nothing to continue from.
    Failed,
}

/// The request a viewer-request function returned, as the edge continues with it.
#[derive(Debug)]
pub struct Request {
    /// `request.uri` — the viewer path the edge keys on and asks the origin for.
    pub uri: String,
    /// `request.querystring`, rendered back as a query string.
    pub query: String,
    /// `request.headers` and `request.cookies`, rendered back as the request's headers.
    pub headers: Vec<(String, String)>,
}

/// The response a function generated itself, as the edge serves it.
#[derive(Debug)]
pub struct Generated {
    /// `response.statusCode`.
    pub status: u16,
    /// `response.headers` and `response.cookies`, rendered back as the answer's headers.
    pub headers: Vec<(String, String)>,
    /// The bytes `response.body` states, or none for a response that states no body.
    pub body: Vec<u8>,
}

/// Run the behaviour's viewer-REQUEST function over `event` and read what it returned.
///
/// No store lock is held across this: the code is the caller's own and runs on a blocking
/// thread ([`run_function`]).
pub async fn run_viewer_request(associated: &Associated, event: &Event<'_>) -> Rewritten {
    let object = event.object(VIEWER_REQUEST, None);
    let Some(returned) = returned_event(associated, VIEWER_REQUEST, object).await else {
        return Rewritten::Failed;
    };
    // "A response object must contain `statusCode`": which of the two a function returned is
    // decided by the object itself, so a viewer-request function that built a response is read
    // as having generated one.
    match member(&returned, RESPONSE_MEMBER) {
        Some(response) => match generated(response) {
            Some(generated) => Rewritten::Response(generated),
            None => Rewritten::Failed,
        },
        None => match member(&returned, REQUEST_MEMBER) {
            Some(request) => Rewritten::Request(Request {
                uri: text(request, URI_MEMBER).unwrap_or_else(|| event.uri.to_string()),
                query: query_string(member(request, QUERYSTRING_MEMBER)),
                headers: request_headers(request, &event.headers),
            }),
            None => Rewritten::Failed,
        },
    }
}

/// Run the behaviour's viewer-RESPONSE function over `event` and the answer the edge is about
/// to serve, and hand back the answer the function returned.
///
/// `None` is a function that produced no response object to serve.
pub async fn run_viewer_response(
    associated: &Associated,
    event: &Event<'_>,
    response: &RtResponse,
) -> Option<RtResponse> {
    let object = event.object(VIEWER_RESPONSE, Some(response_object(response)));
    let returned = returned_event(associated, VIEWER_RESPONSE, object).await?;
    // A viewer-response function returns the RESPONSE: an object with no `statusCode` is no
    // response at all, and leaves nothing to serve.
    let returned = member(&returned, RESPONSE_MEMBER)?;
    let status = status_code(returned)?;
    let mut served = RtResponse::new(
        ::aws_smithy_runtime_api::http::StatusCode::try_from(status).ok()?,
        ::aws_smithy_types::body::SdkBody::from(
            response.body().bytes().map(<[u8]>::to_vec).unwrap_or_default(),
        ),
    );
    for (name, value) in response_headers(returned, response) {
        let _ = served.headers_mut().try_append(name, value);
    }
    Some(served)
}

/// Run one association's code over `object` and read the event object it returned.
async fn returned_event(associated: &Associated, event_type: &str, object: String) -> Option<Json> {
    let run = run_function(associated.code.clone(), object).await;
    for line in &run.logs {
        // What the function logged, where a CloudFront function's log lines go: the account's
        // own log destination is the standard logging plane, and a line is the caller's own
        // content, so it is recorded at the level a line of a caller's code is.
        ::tracing::debug!(
            function = %associated.arn,
            event_type = %event_type,
            line = %line,
            "a CloudFront function at the edge logged a line"
        );
    }
    match run.outcome {
        FunctionOutcome::Returned(output) => match ::serde_json::from_str::<Json>(&output) {
            Ok(returned) => Some(returned),
            Err(error) => {
                ::tracing::warn!(
                    function = %associated.arn,
                    event_type = %event_type,
                    %error,
                    "the event object a CloudFront function returned could not be read"
                );
                None
            }
        },
        FunctionOutcome::Failed(message) => {
            ::tracing::warn!(
                function = %associated.arn,
                event_type = %event_type,
                detail = %message,
                "the CloudFront function this cache behaviour associates failed, so the edge \
                 has no event object to continue from"
            );
            None
        }
        FunctionOutcome::Unanswered => {
            // The code ran and took no decision. A function CloudFront runs on a viewer event
            // answers with the event object it continues from ("returns the function's result,
            // the modified event object"), so a run that answers nothing leaves the edge with
            // neither a request nor a response — the same place a failed run leaves it.
            ::tracing::warn!(
                function = %associated.arn,
                event_type = %event_type,
                "the CloudFront function this cache behaviour associates came back without an \
                 event object, so the edge has nothing to continue from"
            );
            None
        }
        FunctionOutcome::Unavailable(reason) => {
            ::tracing::warn!(
                function = %associated.arn,
                event_type = %event_type,
                detail = %reason,
                "the CloudFront function this cache behaviour associates did not run, so the \
                 edge has no event object to continue from"
            );
            None
        }
    }
}

/// The object `member` names, when the event object carries one.
fn member<'j>(event: &'j Json, member: &str) -> Option<&'j Json> {
    event
        .get(member)
        .filter(|value| matches!(value, Json::Object(_)))
}

/// The response object of the answer the edge is about to serve, as the event object states
/// it.
fn response_object(response: &RtResponse) -> Json {
    let mut object = Map::new();
    object.insert(
        STATUS_CODE_MEMBER.to_string(),
        Json::Number(response.status().as_u16().into()),
    );
    if let Some(reason) = status_reason(response.status().as_u16()) {
        object.insert(
            STATUS_DESCRIPTION_MEMBER.to_string(),
            Json::String(reason.to_string()),
        );
    }
    let headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.to_string()))
        .collect();
    object.insert(
        HEADERS_MEMBER.to_string(),
        named_values(
            headers
                .iter()
                .filter(|(name, _)| name != SET_COOKIE_HEADER)
                .map(|(name, value)| (name.clone(), value.clone())),
        ),
    );
    object.insert(COOKIES_MEMBER.to_string(), set_cookies_of(&headers));
    Json::Object(object)
}

/// The reason phrase of `status`, which the response object states beside the code.
///
/// The phrases RFC 9110 registers for the statuses an origin answers and CloudFront
/// generates; a status with none is carried by its code alone, which is what the member
/// documents ("the HTTP status description").
fn status_reason(status: u16) -> Option<&'static str> {
    ::http::StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
}

/// The request's headers as the edge continues with them: what the function returned, with
/// the read-only ones kept as the viewer request had them and the cookies rendered back into
/// one `Cookie` header.
fn request_headers(request: &Json, viewer: &[(String, String)]) -> Vec<(String, String)> {
    let mut headers = flat_values(
        member(request, HEADERS_MEMBER),
        &READ_ONLY_REQUEST_HEADERS,
        Case::Lowercased,
    );
    for name in READ_ONLY_REQUEST_HEADERS {
        for (held, value) in viewer.iter().filter(|(held, _)| held == name) {
            headers.push((held.clone(), value.clone()));
        }
    }
    let cookies = cookie_header(member(request, COOKIES_MEMBER));
    if !cookies.is_empty() {
        headers.push((COOKIE_HEADER.to_string(), cookies));
    }
    headers
}

/// The answer's headers as the viewer receives them: what the function returned, with the
/// read-only ones kept as the edge had them and the cookies rendered back into `Set-Cookie`
/// headers.
fn response_headers(returned: &Json, response: &RtResponse) -> Vec<(String, String)> {
    let mut headers = flat_values(
        member(returned, HEADERS_MEMBER),
        &READ_ONLY_RESPONSE_HEADERS,
        Case::Lowercased,
    );
    for name in READ_ONLY_RESPONSE_HEADERS {
        for value in response.headers().get_all(name) {
            headers.push((name.to_string(), value.to_string()));
        }
    }
    headers.extend(set_cookie_headers(member(returned, COOKIES_MEMBER)));
    headers
}

/// The response a function generated: its status, its headers and the bytes it stated.
fn generated(response: &Json) -> Option<Generated> {
    let status = status_code(response)?;
    let mut headers = flat_values(member(response, HEADERS_MEMBER), &[], Case::Lowercased);
    headers.extend(set_cookie_headers(member(response, COOKIES_MEMBER)));
    Some(Generated {
        status,
        headers,
        body: body_bytes(response.get(BODY_MEMBER)),
    })
}

/// `response.statusCode` as a status a viewer can be answered with.
fn status_code(response: &Json) -> Option<u16> {
    let status = response.get(STATUS_CODE_MEMBER)?;
    let status = match status {
        Json::Number(number) => number.as_u64()?,
        // A status the code spelled as text, which is the same status named in the other of
        // the two ways JSON carries a number.
        Json::String(text) => text.parse().ok()?,
        _ => return None,
    };
    (MIN_STATUS..=MAX_STATUS)
        .contains(&status)
        .then(|| u16::try_from(status).ok())
        .flatten()
}

/// The bytes `response.body` states: the data as it stands, or the data its `base64` encoding
/// decodes to.
fn body_bytes(body: Option<&Json>) -> Vec<u8> {
    match body {
        // "body: ... a string" — the bytes of the text as it stands.
        Some(Json::String(data)) => data.as_bytes().to_vec(),
        Some(object @ Json::Object(_)) => {
            let data = text(object, DATA_MEMBER).unwrap_or_default();
            match text(object, ENCODING_MEMBER).as_deref() {
                Some(ENCODING_BASE64) => {
                    ::base64::Engine::decode(&::base64::engine::general_purpose::STANDARD, &data)
                        .unwrap_or_default()
                }
                // "text" and a response stating no encoding, which is the same thing: the data
                // as it stands.
                Some(ENCODING_TEXT) | None => data.into_bytes(),
                // An encoding this event object does not define decodes to nothing.
                Some(_) => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// The text `member` carries, when it carries text.
fn text(object: &Json, member: &str) -> Option<String> {
    object
        .get(member)
        .and_then(Json::as_str)
        .map(ToString::to_string)
}

/// One name's values, as the event object states them: the first under `value`, and every one
/// of them under `multiValue` when the name carries more than one.
fn named_values(pairs: impl Iterator<Item = (String, String)>) -> Json {
    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    for (name, value) in pairs {
        match grouped.iter_mut().find(|(held, _)| *held == name) {
            Some((_, values)) => values.push(value),
            None => grouped.push((name, vec![value])),
        }
    }
    let mut object = Map::new();
    for (name, values) in grouped {
        let mut entry = Map::new();
        entry.insert(
            VALUE_MEMBER.to_string(),
            Json::String(values.first().cloned().unwrap_or_default()),
        );
        if values.len() > 1 {
            entry.insert(
                MULTI_VALUE_MEMBER.to_string(),
                Json::Array(
                    values
                        .iter()
                        .map(|value| {
                            let mut held = Map::new();
                            held.insert(VALUE_MEMBER.to_string(), Json::String(value.clone()));
                            Json::Object(held)
                        })
                        .collect(),
                ),
            );
        }
        object.insert(name, Json::Object(entry));
    }
    Json::Object(object)
}

/// Whether the names of the part being read back are case-insensitive, as a HEADER name is
/// (RFC 9110), or carry the case the request stated them in.
///
/// A query-string parameter and a cookie are named case-SENSITIVELY — `Pick` and `pick` are
/// two different parameters ([`forwarding::NameSet::contains`] reads them that way) — so the
/// name a function returned for one is put back on the wire exactly as the object spelled it,
/// and only a header name is folded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    /// A header name: folded to lower case, which is how the edge carries one.
    Lowercased,
    /// A query-string parameter or a cookie name: kept as stated.
    AsStated,
}

/// The `(name, value)` pairs a `value`/`multiValue` object carries, flattened back out — each
/// name once per value it holds — leaving out the names in `withheld`.
fn flat_values(object: Option<&Json>, withheld: &[&str], case: Case) -> Vec<(String, String)> {
    let Some(Json::Object(members)) = object else {
        return Vec::new();
    };
    let mut pairs = Vec::new();
    for (name, entry) in members {
        let name = match case {
            Case::Lowercased => name.to_ascii_lowercase(),
            Case::AsStated => name.clone(),
        };
        if withheld.iter().any(|held| *held == name) {
            continue;
        }
        match entry.get(MULTI_VALUE_MEMBER) {
            Some(Json::Array(values)) => {
                for value in values {
                    if let Some(value) = value.get(VALUE_MEMBER).and_then(Json::as_str) {
                        pairs.push((name.clone(), value.to_string()));
                    }
                }
            }
            _ => {
                if let Some(value) = value_of(entry) {
                    pairs.push((name.clone(), value));
                }
            }
        }
    }
    pairs
}

/// The `value` of one entry of a `value`/`multiValue` object: the text it states, or the text
/// a code that stated the value directly spelled.
fn value_of(entry: &Json) -> Option<String> {
    match entry {
        Json::Object(_) => match entry.get(VALUE_MEMBER) {
            Some(Json::String(value)) => Some(value.clone()),
            Some(Json::Number(value)) => Some(value.to_string()),
            Some(Json::Bool(value)) => Some(value.to_string()),
            _ => None,
        },
        // A value the code assigned as a bare string rather than as the `{ value }` object the
        // event carries, which is a shape the engine will have rendered as it stands.
        Json::String(value) => Some(value.clone()),
        _ => None,
    }
}

/// The query string's `name=value` pairs, in the order the viewer sent them.
///
/// Split by the one reader of a query string this service has ([`forwarding::query_pairs`]),
/// so the names a function is shown are the names a policy keys and forwards on.
fn query_pairs(query: &str) -> impl Iterator<Item = (String, String)> + '_ {
    forwarding::query_pairs(query).into_iter().map(stated_value)
}

/// The name and the value of one `name=value` pair, which carries the whole pair as the
/// viewer spelled it: the value is what follows the name's `=`, and the empty string for a
/// name the viewer sent without one.
fn stated_value(pair: forwarding::Pair<'_>) -> (String, String) {
    let value = pair
        .text
        .strip_prefix(pair.name)
        .and_then(|rest| rest.strip_prefix('='))
        .unwrap_or_default();
    (pair.name.to_string(), value.to_string())
}

/// The query string the edge continues with, rendered back from the event object's
/// `querystring`.
fn query_string(querystring: Option<&Json>) -> String {
    flat_values(querystring, &[], Case::AsStated)
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// The viewer's cookies, as the event object's `cookies` member states them.
///
/// Split by the one reader of a `Cookie` header this service has
/// ([`forwarding::cookie_pairs`]).
fn cookies_of(headers: &[(String, String)]) -> Json {
    let pairs = headers
        .iter()
        .filter(|(name, _)| name == COOKIE_HEADER)
        .flat_map(|(_, value)| {
            forwarding::cookie_pairs(value)
                .into_iter()
                .map(stated_value)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    named_values(pairs.into_iter())
}

/// The `Cookie` header the edge continues with, rendered back from the event object's
/// `cookies`.
fn cookie_header(cookies: Option<&Json>) -> String {
    flat_values(cookies, &[], Case::AsStated)
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The answer's cookies, as the event object's response `cookies` member states them: the
/// value of each `Set-Cookie` beside the attributes that cookie carries.
fn set_cookies_of(headers: &[(String, String)]) -> Json {
    let mut object = Map::new();
    for (_, header) in headers.iter().filter(|(name, _)| name == SET_COOKIE_HEADER) {
        let (pair, attributes) = match header.split_once(';') {
            Some((pair, attributes)) => (pair.trim(), attributes.trim()),
            None => (header.trim(), ""),
        };
        let (name, value) = match pair.split_once('=') {
            Some((name, value)) => (name.trim(), value.trim()),
            None => (pair, ""),
        };
        let mut entry = Map::new();
        entry.insert(VALUE_MEMBER.to_string(), Json::String(value.to_string()));
        entry.insert(
            ATTRIBUTES_MEMBER.to_string(),
            Json::String(attributes.to_string()),
        );
        object.insert(name.to_string(), Json::Object(entry));
    }
    Json::Object(object)
}

/// The `Set-Cookie` headers the viewer receives, rendered back from the event object's
/// response `cookies`.
fn set_cookie_headers(cookies: Option<&Json>) -> Vec<(String, String)> {
    let Some(Json::Object(members)) = cookies else {
        return Vec::new();
    };
    members
        .iter()
        .map(|(name, entry)| {
            let value = value_of(entry).unwrap_or_default();
            let attributes = text(entry, ATTRIBUTES_MEMBER).unwrap_or_default();
            let header = if attributes.is_empty() {
                format!("{name}={value}")
            } else {
                format!("{name}={value}; {attributes}")
            };
            (SET_COOKIE_HEADER.to_string(), header)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::aws_smithy_types::body::SdkBody;

    /// A viewer request carrying `headers`, addressed at `uri`.
    fn request(headers: &[(&str, &str)]) -> RtRequest {
        let mut req = RtRequest::new(SdkBody::empty());
        req.set_uri("/obj".to_string()).expect("a valid URI");
        for (name, value) in headers {
            req.headers_mut()
                .append((*name).to_string(), (*value).to_string());
        }
        req
    }

    /// The event object for a viewer-request event over `req`.
    fn event_object(req: &RtRequest, query: &str) -> Json {
        let event = Event::of_request("E123", "d111.cloudfront.net", "cf-id", "198.51.100.11", req, "/obj", query);
        ::serde_json::from_str(&event.object(VIEWER_REQUEST, None)).expect("the event is JSON")
    }

    #[test]
    fn the_event_object_states_the_request_cloudfront_passes_a_function() {
        let req = request(&[
            ("host", "d111.cloudfront.net"),
            ("accept", "*/*"),
            ("cookie", "session=abc; theme=dark"),
        ]);
        let event = event_object(&req, "a=1&b=2&a=3");
        assert_eq!(
            event[VERSION_MEMBER], Json::String(EVENT_VERSION.to_string()),
            "case {:?}: expected the event version {EVENT_VERSION:?}, got {event:?}",
            "a viewer request"
        );
        assert_eq!(
            event[CONTEXT_MEMBER][EVENT_TYPE_MEMBER],
            Json::String(VIEWER_REQUEST.to_string()),
            "case {:?}: expected the viewer-request event type, got {event:?}",
            "a viewer request"
        );
        assert_eq!(
            event[VIEWER_MEMBER][IP_MEMBER],
            Json::String("198.51.100.11".to_string()),
            "case {:?}: expected the viewer's address, got {event:?}",
            "a viewer request"
        );
        let request = &event[REQUEST_MEMBER];
        assert_eq!(
            request[URI_MEMBER], Json::String("/obj".to_string()),
            "case {:?}: expected the viewer path, got {request:?}",
            "a viewer request"
        );
        // A repeated query-string name carries every value under `multiValue`.
        assert_eq!(
            request[QUERYSTRING_MEMBER]["a"][MULTI_VALUE_MEMBER]
                .as_array()
                .map(Vec::len),
            Some(2),
            "case {:?}: expected both values of a repeated parameter, got {request:?}",
            "a repeated query-string name"
        );
        // The cookies travel in `cookies`, and not in `headers`.
        assert_eq!(
            request[COOKIES_MEMBER]["session"][VALUE_MEMBER],
            Json::String("abc".to_string()),
            "case {:?}: expected the cookie's value, got {request:?}",
            "a viewer that sent cookies"
        );
        assert!(
            request[HEADERS_MEMBER].get(COOKIE_HEADER).is_none(),
            "case {:?}: expected no cookie header in the event's headers, got {request:?}",
            "a viewer that sent cookies"
        );
    }

    #[test]
    fn a_name_keeps_its_case_where_the_part_is_case_sensitive() {
        // A HEADER name is case-insensitive (RFC 9110), so the edge carries it folded; a
        // query-string parameter and a cookie are named case-SENSITIVELY, so the name the
        // event object states is what goes back on the wire. Rows differ along the part being
        // read back and along the case the name was stated in.
        let returned: Json = ::serde_json::from_str(
            "{\"headers\":{\"X-Mixed-Case\":{\"value\":\"header\"}},\
             \"querystring\":{\"Pick\":{\"value\":\"other\"},\"flag\":{\"value\":\"\"}},\
             \"cookies\":{\"Session\":{\"value\":\"abc\"}}}",
        )
        .expect("the returned object is JSON");
        let headers = request_headers(&returned, &[]);
        assert!(
            headers.contains(&("x-mixed-case".to_string(), "header".to_string())),
            "case {:?}: expected the header name folded, got {headers:?}",
            "a header name stated in mixed case"
        );
        let query = query_string(member(&returned, QUERYSTRING_MEMBER));
        assert!(
            query.contains("Pick=other"),
            "case {:?}: expected the parameter name as stated, got {query:?}",
            "a query-string name stated in mixed case"
        );
        let cookies = cookie_header(member(&returned, COOKIES_MEMBER));
        assert_eq!(
            cookies, "Session=abc",
            "case {:?}: expected the cookie name as stated, got {cookies:?}",
            "a cookie name stated in mixed case"
        );
        // And a request the function did not change round-trips: the viewer's own query string
        // and cookies come back exactly as they were sent.
        let req = request(&[("cookie", "Session=abc; theme=dark")]);
        let event = Event::of_request("E1", "d1.cloudfront.net", "cf", "198.51.100.11", &req, "/o", "Pick=other&flag=");
        let object: Json =
            ::serde_json::from_str(&event.object(VIEWER_REQUEST, None)).expect("the event is JSON");
        let round_tripped = query_string(member(&object[REQUEST_MEMBER], QUERYSTRING_MEMBER));
        assert_eq!(
            round_tripped, "Pick=other&flag=",
            "case {:?}: expected the viewer's own query string, got {round_tripped:?}",
            "a query string no function changed"
        );
        let round_tripped = cookie_header(member(&object[REQUEST_MEMBER], COOKIES_MEMBER));
        assert_eq!(
            round_tripped, "Session=abc; theme=dark",
            "case {:?}: expected the viewer's own cookies, got {round_tripped:?}",
            "cookies no function changed"
        );
    }

    #[test]
    fn a_read_only_request_header_keeps_the_viewers_value() {
        // Rows differ along what the function returned for the header: the read-only `host`,
        // a header it changed, a header it added and a header it dropped.
        let viewer = vec![
            ("host".to_string(), "d111.cloudfront.net".to_string()),
            ("accept".to_string(), "*/*".to_string()),
            ("x-dropped".to_string(), "gone".to_string()),
        ];
        let returned: Json = ::serde_json::from_str(
            "{\"headers\":{\"host\":{\"value\":\"attacker.example\"},\
             \"accept\":{\"value\":\"text/plain\"},\"x-added\":{\"value\":\"new\"}}}",
        )
        .expect("the returned object is JSON");
        let headers = request_headers(&returned, &viewer);
        let value = |name: &str| {
            headers
                .iter()
                .find(|(held, _)| held == name)
                .map(|(_, value)| value.clone())
        };
        let cases = [
            ("a read-only header", "host", Some("d111.cloudfront.net")),
            ("a header the function changed", "accept", Some("text/plain")),
            ("a header the function added", "x-added", Some("new")),
            ("a header the function dropped", "x-dropped", None),
        ];
        for (case, name, expected) in cases {
            let actual = value(name);
            assert_eq!(
                actual.as_deref(),
                expected,
                "case {case:?} ({name:?}): expected {expected:?}, got {actual:?}"
            );
        }
    }

    #[test]
    fn a_status_outside_the_http_range_is_no_response() {
        // Rows differ along what the response object stated for its status: a code, a code as
        // text, codes at each published bound, one past each bound, and no code at all.
        let cases: [(&str, &str, Option<u16>); 7] = [
            ("a status the origin answers", "{\"statusCode\":200}", Some(200)),
            ("a status spelled as text", "{\"statusCode\":\"302\"}", Some(302)),
            ("the lowest status", "{\"statusCode\":100}", Some(100)),
            ("the highest status", "{\"statusCode\":599}", Some(599)),
            ("one below the lowest", "{\"statusCode\":99}", None),
            ("one above the highest", "{\"statusCode\":600}", None),
            ("an object stating no status", "{\"headers\":{}}", None),
        ];
        for (case, object, expected) in cases {
            let object: Json = ::serde_json::from_str(object).expect("the object is JSON");
            let actual = status_code(&object);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the status {expected:?}, got {actual:?}"
            );
        }
    }

    #[test]
    fn a_generated_response_carries_the_bytes_it_stated() {
        // Rows differ along the form `body` took: the text form, a bare string, base64, no
        // body at all, and an encoding the event object does not define.
        let cases: [(&str, &str, &[u8]); 5] = [
            (
                "the text form",
                "{\"statusCode\":200,\"body\":{\"encoding\":\"text\",\"data\":\"hello\"}}",
                b"hello",
            ),
            ("a bare string", "{\"statusCode\":200,\"body\":\"hello\"}", b"hello"),
            (
                "the base64 form",
                "{\"statusCode\":200,\"body\":{\"encoding\":\"base64\",\"data\":\"aGVsbG8=\"}}",
                b"hello",
            ),
            ("no body", "{\"statusCode\":204}", b""),
            (
                "an encoding the event object does not define",
                "{\"statusCode\":200,\"body\":{\"encoding\":\"rot13\",\"data\":\"uryyb\"}}",
                b"",
            ),
        ];
        for (case, object, expected) in cases {
            let object: Json = ::serde_json::from_str(object).expect("the object is JSON");
            let generated = generated(&object).expect("the object states a status");
            assert_eq!(
                generated.body, expected,
                "case {case:?}: expected the bytes {expected:?}, got {:?}",
                generated.body
            );
        }
    }

    #[test]
    fn the_response_object_carries_the_answers_headers_and_cookies() {
        let mut response = RtResponse::new(
            ::aws_smithy_runtime_api::http::StatusCode::try_from(200).expect("a status"),
            SdkBody::from(&b"bytes"[..]),
        );
        let _ = response
            .headers_mut()
            .try_insert("content-type", "text/plain".to_string());
        let _ = response
            .headers_mut()
            .try_append("set-cookie", "id=7; Path=/".to_string());
        let object = response_object(&response);
        assert_eq!(
            object[STATUS_CODE_MEMBER], Json::Number(200.into()),
            "case {:?}: expected the answer's status, got {object:?}",
            "an answer with a cookie"
        );
        assert_eq!(
            object[STATUS_DESCRIPTION_MEMBER], Json::String("OK".to_string()),
            "case {:?}: expected the status description, got {object:?}",
            "an answer with a cookie"
        );
        assert_eq!(
            object[COOKIES_MEMBER]["id"][ATTRIBUTES_MEMBER],
            Json::String("Path=/".to_string()),
            "case {:?}: expected the cookie's attributes, got {object:?}",
            "an answer with a cookie"
        );
        // And the headers the function returns are rendered back, with the read-only ones kept
        // as the edge had them.
        let returned: Json = ::serde_json::from_str(
            "{\"headers\":{\"content-type\":{\"value\":\"text/html\"},\
             \"content-length\":{\"value\":\"999\"}},\
             \"cookies\":{\"id\":{\"value\":\"8\",\"attributes\":\"Secure\"}}}",
        )
        .expect("the returned object is JSON");
        let headers = response_headers(&returned, &response);
        assert!(
            headers.contains(&("content-type".to_string(), "text/html".to_string())),
            "case {:?}: expected the function's content type, got {headers:?}",
            "a function that changed the media type"
        );
        assert!(
            !headers
                .iter()
                .any(|(name, value)| name == "content-length" && value == "999"),
            "case {:?}: expected no function-stated content length, got {headers:?}",
            "a function that changed a read-only header"
        );
        assert!(
            headers.contains(&("set-cookie".to_string(), "id=8; Secure".to_string())),
            "case {:?}: expected the cookie the function stated, got {headers:?}",
            "a function that changed a cookie"
        );
    }
}
