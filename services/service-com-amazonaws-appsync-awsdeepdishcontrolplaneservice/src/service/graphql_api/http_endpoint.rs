// SPDX-License-Identifier: CC0-1.0

//! What a resolver step whose data source is of the `HTTP` type DOES: it SENDS the HTTP
//! request its request half rendered to the endpoint the data source was configured with
//! (`HttpDataSourceConfig$endpoint`, "The HTTP URL endpoint"), and the response that endpoint
//! answered is the result the step's response half is handed.
//!
//! The request half of such a step returns AppSync's own HTTP request object: the `method` the
//! call is made with, the `resourcePath` it is made at, and `params` carrying the `query`
//! parameters, the `headers` and the `body` it carries. The result bound into `$ctx.result` is
//! what came back — `statusCode`, `headers` and `body` — whatever that status is: a backend's
//! non-2xx answer is the step's RESULT rather than its failure, which is what lets a response
//! definition read the status and the body its endpoint answered with.
//!
//! Three things make this a real call rather than a reported one:
//!
//! * the request is SENT, over the emulator's own in-process transport (the callback handle
//!   this plane holds), so an endpoint this emulator serves answers it and what the field
//!   resolves to is that service's own answer, with no connection opened and nothing invented:
//!   the method, the path, the query, the headers and the body are the ones the step's
//!   definition rendered, forwarded as they stand. The configured endpoint is ADDRESSING for
//!   that transport rather than a host this process connects to — the request is dispatched
//!   back into this same emulator, so nothing outside this process is reachable through it, and
//!   nothing of the GraphQL CALLER's own credential travels on it either: the fields the
//!   transport owns are not a request object's to set ([`RESERVED_FIELDS`]), and what the call
//!   presents is the credential the in-process transport hands this endpoint;
//! * where the data source's `authorizationConfig` is `AWS_IAM` the request is SIGV4-SIGNED for
//!   the configured `awsIamConfig.signingServiceName` in its `signingRegion` — which is both
//!   what admits the call at an AWS API and what ROUTES it to that API, since a signed AWS
//!   request names the service it is for in its own credential scope. That is the documented
//!   way to reach an AWS service API from a resolver without a function in between, and it is
//!   why such a step reaches the very service this emulator mounts;
//! * such a call is made AS the role the data source names (`DataSource$serviceRoleArn`: "The
//!   system assumes this role when accessing the data source"): the request is signed with a
//!   SESSION of that role ([`super::role_session`]), so the service it reaches resolves that
//!   session as its caller and ITS OWN authorization decides the call. A role whose policies do
//!   not allow the action is refused by the called service, in that service's own words and with
//!   its own status, and the step is handed that answer exactly as it is handed any other — so a
//!   definition reading `$ctx.result.statusCode` sees the denial the service stated, and nothing
//!   of the refusal is invented here.
//!
//! Nothing of the stored record is changed here: this module reads the data source's
//! configuration and places one call.

use ::serde_json::{json, Map as JsonMap, Value as Json};

use super::PlaneContext;

/// The `HTTP` data source a step resolves against, as the step's own record carries it.
#[derive(Debug, Clone, Copy)]
pub struct Configured<'a> {
    /// The data source's name, which an error of the step names it by.
    pub name: &'a str,
    /// `httpConfig.endpoint` — "The HTTP URL endpoint" every request of this data source is
    /// sent to.
    pub endpoint: Option<&'a str>,
    /// `httpConfig.authorizationConfig.authorizationType` — the one value the model declares
    /// is `AWS_IAM` ([`AWS_IAM_AUTHORIZATION`]), which is what makes the outbound request a
    /// SIGNED one. `None` for a data source whose endpoint requires no authorization.
    pub authorization_type: Option<&'a str>,
    /// `awsIamConfig.signingRegion` — "The signing Amazon Web Services Region for IAM
    /// authorization", or `None` for a configuration that names none, where the API's own
    /// Region is signed for.
    pub signing_region: Option<&'a str>,
    /// `awsIamConfig.signingServiceName` — "The signing service name for IAM authorization".
    pub signing_service_name: Option<&'a str>,
    /// `serviceRoleArn`, the role AppSync makes a signed call under, or `None` for a data
    /// source that names none.
    pub service_role_arn: Option<&'a str>,
}

/// Send the request `requested` describes to `source`'s endpoint and answer the result the
/// step's response half is handed, or what the step did not resolve with — a [`Refused`],
/// whose type is what the step's own `ctx.error.type` carries.
pub async fn answered(
    context: &PlaneContext,
    source: &Configured<'_>,
    requested: &Json,
) -> Result<Json, Refused> {
    let Some(endpoint) = source
        .endpoint
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty())
    else {
        return Err(Refused::request(format!("{NO_ENDPOINT} {}.", source.name)));
    };
    let call = Call::read(requested).map_err(Refused::request)?;
    let addressed = call.addressed(endpoint).map_err(Refused::request)?;
    let mut request = call.http_request(&addressed).map_err(Refused::request)?;
    if signs_its_requests(source) {
        let Some(signing_name) = source
            .signing_service_name
            .map(str::trim)
            .filter(|name| !name.is_empty())
        else {
            return Err(Refused::request(format!(
                "{NO_SIGNING_SERVICE} {}.",
                source.name
            )));
        };
        let region = source
            .signing_region
            .map(str::trim)
            .filter(|region| !region.is_empty())
            .unwrap_or(&context.region);
        // The call is made AS the data source's role, so it presents a session of that role and
        // the service it reaches decides it. A role nothing can be shown about leaves the call
        // signed with the credential the in-process transport hands this endpoint, which is what
        // every other call between two services of this emulator presents
        // ([`super::role_session`]).
        let session = source
            .service_role_arn
            .map(str::trim)
            .filter(|role_arn| !role_arn.is_empty())
            .and_then(|role_arn| super::role_session::minted(context, role_arn));
        signed(&mut request, signing_name, region, context, session.as_ref())
            .await
            .map_err(|error| Refused::unmade(&format!("{NOT_SIGNED} {error}")))?;
        let answered = sent(context, request).await;
        // The session was minted for THIS call: it is forgotten on every path out of it, so a
        // credential nothing presents any more is valid nowhere.
        if let Some(session) = &session {
            super::role_session::forget(context, &session.access_key_id);
        }
        return answered;
    }
    sent(context, request).await
}

/// SigV4-sign one outbound request, as the identity this call is made as.
///
/// The session of the data source's role where one was minted for it — whose token travels in
/// the header a temporary credential is served with, put on the request BEFORE the signature so
/// it is signed with the rest — and otherwise the credential the framework hands this emulator's
/// handlers. Either way the signature is the tree's one in-process signature
/// (`service_common::in_process_sigv4`), so the emulator's own verifier computes the same one.
async fn signed(
    request: &mut ::http::Request<Vec<u8>>,
    signing_name: &str,
    region: &str,
    context: &PlaneContext,
    session: Option<&super::role_session::RoleSession>,
) -> Result<(), String> {
    let Some(session) = session else {
        return ::service_common::in_process_sigv4::sign_callback_request(
            request,
            signing_name,
            region,
            &context.callback,
        )
        .await;
    };
    request.headers_mut().insert(
        SESSION_TOKEN_FIELD,
        ::http::HeaderValue::from_str(&session.session_token)
            .map_err(|error| error.to_string())?,
    );
    ::service_common::in_process_sigv4::sign_in_process_request_as(
        request,
        signing_name,
        region,
        &session.access_key_id,
        &session.secret_access_key,
    )
    .await
}

/// Whether this data source's outbound requests are SIGNED: "the authorization configuration
/// in case the HTTP endpoint requires authorization" (`HttpDataSourceConfig$authorizationConfig`)
/// states `AWS_IAM`, the one authorization type the model declares.
fn signs_its_requests(source: &Configured<'_>) -> bool {
    source
        .authorization_type
        .map(str::trim)
        .is_some_and(|declared| declared == AWS_IAM_AUTHORIZATION)
}

/// Why a step resolving against an `HTTP` data source did not resolve: the sentence it states,
/// and the `errorType` it is of.
///
/// AppSync reports both to the step's own `ctx.error` (`message` and `type`), which is what a
/// definition reads to decide the field — and what it re-raises with where it calls
/// `util.error(ctx.error.message, ctx.error.type)`.
#[derive(Debug, Clone)]
pub struct Refused {
    pub message: String,
    pub error_type: String,
}

impl Refused {
    /// A request this data source cannot send: the step's own definition produced it, so it is
    /// an error of that CODE rather than anything an endpoint answered.
    fn request(message: String) -> Self {
        Self {
            message,
            error_type: REQUEST_ERROR_TYPE.to_string(),
        }
    }

    /// A call that was never SENT, or whose answer this endpoint could not carry: there is no
    /// response to bind, so the step is reported as one whose request could not be placed.
    fn unmade(reason: &str) -> Self {
        Self {
            message: reason.to_string(),
            error_type: UNSENT_ERROR_TYPE.to_string(),
        }
    }
}

/// The ONE request a step of this data source sends, as its request half described it.
#[derive(Debug, Clone)]
struct Call {
    /// The HTTP method, as the request object's `method` names it.
    method: String,
    /// `resourcePath`, the path the call is made at, relative to the endpoint's own.
    resource_path: String,
    /// `params.query`, one pair per value the request states for a parameter.
    query: Vec<(String, String)>,
    /// `params.headers`, one pair per value the request states for a field.
    headers: Vec<(String, String)>,
    /// `params.body`, the bytes the request carries.
    body: Vec<u8>,
}

impl Call {
    /// The request `requested` describes, or the sentence the step is refused with — which
    /// LOCATES the offending value by its member path in the request the step's own definition
    /// produced, because that is the value a client has to correct.
    fn read(requested: &Json) -> Result<Self, String> {
        let Some(members) = requested.as_object() else {
            return Err(format!("{A_REQUEST_IS_AN_OBJECT} {requested}."));
        };
        let method = members
            .get(METHOD_MEMBER)
            .and_then(Json::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_ascii_uppercase();
        if !METHODS.contains(&method.as_str()) {
            return Err(format!(
                "{ONE_OF_THE_METHODS} {}, and this request carries {method:?} at {}.",
                METHODS.join(", "),
                member_path(&[METHOD_MEMBER])
            ));
        }
        let resource_path = members
            .get(RESOURCE_PATH_MEMBER)
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .unwrap_or(ROOT_PATH)
            .to_string();
        let params = members.get(PARAMS_MEMBER).filter(|params| !params.is_null());
        if let Some(params) = params {
            if !params.is_object() {
                return Err(format!(
                    "{PARAMS_ARE_AN_OBJECT} {}.",
                    member_path(&[PARAMS_MEMBER])
                ));
            }
        }
        let query = read_pairs(params, QUERY_MEMBER)?;
        let headers = read_pairs(params, HEADERS_MEMBER)?;
        let body = read_body(params);
        Ok(Self {
            method,
            resource_path,
            query,
            headers,
            body,
        })
    }

    /// The whole URL this call is sent to: the data source's endpoint, the `resourcePath` under
    /// its own path, and the query the request states.
    ///
    /// Read with the maintained URL parser this crate already reads a URL with, so the endpoint
    /// is resolved rather than concatenated: a `resourcePath` is a path under the endpoint's
    /// own, and a path that carries a query string of its own keeps it, with the request's
    /// `params.query` appended after it.
    fn addressed(&self, endpoint: &str) -> Result<String, String> {
        let mut url = ::url::Url::parse(endpoint)
            .ok()
            .filter(::url::Url::has_host)
            .ok_or_else(|| format!("{NOT_AN_ENDPOINT} {endpoint:?}."))?;
        let (path, inline) = match self.resource_path.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (self.resource_path.as_str(), None),
        };
        let base = url.path().trim_end_matches('/').to_string();
        let joined = match path.starts_with('/') {
            true => format!("{base}{path}"),
            false => format!("{base}/{path}"),
        };
        url.set_path(&joined);
        url.set_query(inline.filter(|query| !query.is_empty()));
        if !self.query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (name, value) in &self.query {
                pairs.append_pair(name, value);
            }
        }
        Ok(url.to_string())
    }

    /// This call as the HTTP request that is sent: the method, the whole URL, the fields the
    /// request states and its body.
    ///
    /// The fields the TRANSPORT owns are not a request object's to set ([`RESERVED_FIELDS`]):
    /// the endpoint's own host, the signature the data source's authorization configuration
    /// decides, and the framing of the body.
    fn http_request(&self, addressed: &str) -> Result<::http::Request<Vec<u8>>, String> {
        let uri: ::http::Uri = addressed
            .parse()
            .map_err(|_| format!("{NOT_AN_ENDPOINT} {addressed:?}."))?;
        let host = uri
            .authority()
            .map(|authority| authority.as_str().to_string())
            .ok_or_else(|| format!("{NOT_AN_ENDPOINT} {addressed:?}."))?;
        let method = ::http::Method::from_bytes(self.method.as_bytes())
            .map_err(|_| format!("{ONE_OF_THE_METHODS} {}.", METHODS.join(", ")))?;
        let mut request = ::http::Request::builder()
            .method(method)
            .uri(uri)
            .header(::http::header::HOST, host);
        for (name, value) in &self.headers {
            if RESERVED_FIELDS.contains(&name.to_ascii_lowercase().as_str()) {
                continue;
            }
            let field = ::http::header::HeaderName::try_from(name.as_str())
                .map_err(|_| header_refusal(name))?;
            let carried = ::http::header::HeaderValue::try_from(value.as_str())
                .map_err(|_| header_refusal(name))?;
            request = request.header(field, carried);
        }
        request
            .body(self.body.clone())
            .map_err(|error| format!("{NOT_A_REQUEST} {error}"))
    }
}

/// Send `request` over the emulator's own in-process transport and answer what came back, as
/// the result a step of this data source is handed: the status, the fields and the body.
///
/// A refusal the TRANSPORT itself answered — no endpoint of this emulator serves the address —
/// is not an answer from anything, so it is reported as a call that reached nothing rather than
/// bound into the result as a status and a body nothing produced.
async fn sent(context: &PlaneContext, request: ::http::Request<Vec<u8>>) -> Result<Json, Refused> {
    let (parts, payload) = request.into_parts();
    let request = ::aws_smithy_runtime_api::http::Request::try_from(::http::Request::from_parts(
        parts,
        ::aws_smithy_types::body::SdkBody::from(::bytes::Bytes::from(payload)),
    ))
    .map_err(|error| Refused::unmade(&format!("{NOT_SENT} {error}")))?;
    let answer = ::aws_smithy_runtime_api::client::http::HttpConnector::call(
        &context.callback.http_connector(),
        request,
    )
    .await
    .map_err(|error| {
        Refused::unmade(&format!(
            "{NOT_SENT} {}",
            ::aws_smithy_types::error::display::DisplayErrorContext(&error)
        ))
    })?;
    let status = answer.status().as_u16();
    let mut fields: JsonMap<String, Json> = JsonMap::new();
    for (name, value) in answer.headers().iter() {
        let name = name.to_ascii_lowercase();
        // One field carrying several values is reported as the list form a field with several
        // values travels in, which is how a client reads one off a response.
        let carried = match fields.get(&name).and_then(Json::as_str) {
            Some(held) => format!("{held}{FIELD_VALUE_SEPARATOR}{value}"),
            None => value.to_string(),
        };
        fields.insert(name, json!(carried));
    }
    let payload = answer.body().bytes().unwrap_or_default();
    if ::service_common::in_process_refusal::is_transport_refusal(payload) {
        return Err(Refused::unmade(NOTHING_SERVES_THE_ENDPOINT));
    }
    if payload.len() > MAX_ANSWER_BYTES {
        return Err(Refused::unmade(&format!(
            "{ANSWER_TOO_LARGE} {MAX_ANSWER_BYTES} bytes."
        )));
    }
    Ok(json!({
        STATUS_CODE_MEMBER: status,
        HEADERS_MEMBER: Json::Object(fields),
        BODY_MEMBER: String::from_utf8_lossy(payload),
    }))
}

/// The pairs `params`'s `member` states: one per value, in the order the request states them.
///
/// A value is the text a parameter or a field carries; a request may state several of them for
/// one name, which is the list form both a query parameter and an HTTP field take.
fn read_pairs(params: Option<&Json>, member: &str) -> Result<Vec<(String, String)>, String> {
    let Some(carried) = params
        .and_then(|params| params.get(member))
        .filter(|carried| !carried.is_null())
    else {
        return Ok(Vec::new());
    };
    let Some(members) = carried.as_object() else {
        return Err(format!(
            "{PAIRS_ARE_AN_OBJECT} {}.",
            member_path(&[PARAMS_MEMBER, member])
        ));
    };
    let mut pairs = Vec::new();
    for (name, value) in members {
        let stated = match value {
            Json::Array(values) => values.iter().collect::<Vec<&Json>>(),
            Json::Null => Vec::new(),
            one => vec![one],
        };
        for value in stated {
            let Some(text) = plain_text(value) else {
                return Err(format!(
                    "{A_VALUE_IS_TEXT} {}.",
                    member_path(&[PARAMS_MEMBER, member, name])
                ));
            };
            pairs.push((name.clone(), text));
        }
    }
    Ok(pairs)
}

/// The bytes `params.body` carries: the text of a body stated as text, and the JSON text of one
/// stated as a document — which is what a request object rendering a JSON API's input states,
/// and what AppSync sends for it. A request stating no body carries none.
fn read_body(params: Option<&Json>) -> Vec<u8> {
    match params.and_then(|params| params.get(BODY_MEMBER)) {
        None | Some(Json::Null) => Vec::new(),
        Some(Json::String(text)) => text.clone().into_bytes(),
        Some(document) => document.to_string().into_bytes(),
    }
}

/// `value` as the text a parameter or a field carries, or `None` for one that is not a value at
/// all: a number and a boolean are written as they stand, and an object or a list is no single
/// value.
fn plain_text(value: &Json) -> Option<String> {
    match value {
        Json::String(text) => Some(text.clone()),
        Json::Number(number) => Some(number.to_string()),
        Json::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Where one value of the request the step's definition produced sits, as a refusal locates it.
fn member_path(members: &[&str]) -> String {
    members
        .iter()
        .map(|member| format!("[{member}]"))
        .collect::<Vec<String>>()
        .join("")
}

/// The sentence a field this endpoint cannot carry is refused with, locating that field.
fn header_refusal(name: &str) -> String {
    format!(
        "{A_FIELD_IS_A_HEADER} {}.",
        member_path(&[PARAMS_MEMBER, HEADERS_MEMBER, name])
    )
}

/// The one `AuthorizationType` the model declares (`smithy_query(query="shape",
/// shape="AuthorizationType")`), which is what makes a data source's outbound requests signed.
pub const AWS_IAM_AUTHORIZATION: &str = "AWS_IAM";

/// The methods a request object may state. Source: AppSync's HTTP request object, whose
/// `method` is one of `PUT`, `POST`, `GET`, `DELETE` and `PATCH`.
const METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE"];

/// The path a request that states none is sent to: the endpoint's own.
const ROOT_PATH: &str = "/";

/// The members of the request object this data source is handed, and of the result it answers
/// with.
const METHOD_MEMBER: &str = "method";
const RESOURCE_PATH_MEMBER: &str = "resourcePath";
const PARAMS_MEMBER: &str = "params";
const QUERY_MEMBER: &str = "query";
const HEADERS_MEMBER: &str = "headers";
const BODY_MEMBER: &str = "body";
const STATUS_CODE_MEMBER: &str = "statusCode";

/// The HTTP fields a request object does not set, because the transport owns them: the
/// endpoint's own host, the signature the authorization configuration decides, and the framing
/// of the body.
const RESERVED_FIELDS: &[&str] = &[
    "host",
    "authorization",
    "content-length",
    "connection",
    "transfer-encoding",
];

/// The field the session token of a TEMPORARY credential travels in, which a signed call made
/// as a role session carries beside its signature.
const SESSION_TOKEN_FIELD: &str = "x-amz-security-token";

/// How several values of one response field are reported as one: the separator a field carrying
/// a list of values travels in.
const FIELD_VALUE_SEPARATOR: &str = ", ";

/// The `errorType` a step reports for a request its own data source cannot send: the failure is
/// the resolver's own definition rather than anything an endpoint answered, so it is reported as
/// one of that CODE — the one spelling every calling data source of this endpoint states it in
/// ([`super::data_source_role::REQUEST_FAULT_ERROR_TYPE`]).
const REQUEST_ERROR_TYPE: &str = super::data_source_role::REQUEST_FAULT_ERROR_TYPE;

/// The `errorType` a step reports for a request that was never sent at all. AppSync publishes no
/// wording for it, so this is this emulator's own class, chosen for the one thing a client
/// branches on: that no endpoint answered.
const UNSENT_ERROR_TYPE: &str = "Http:RequestNotSent";

/// How many bytes of an endpoint's answer one step binds into its result.
///
/// An EMULATOR ceiling, not an AWS quota: the answer is held in memory as the step's own
/// `ctx.result`, so the amount this endpoint will carry is declared rather than unbounded.
const MAX_ANSWER_BYTES: usize = 1024 * 1024;

/// The sentences a step that did not send, or could not carry what came back, states.
///
/// AppSync publishes no wording for a request its own data source cannot send, so these are
/// this emulator's own sentences. Each names the rule the request broke, and every one that is
/// about a VALUE of the request ends in the member path of that value ([`member_path`]), so a
/// client reads which value to correct out of the sentence.
const NO_ENDPOINT: &str = "No endpoint is configured for the HTTP data source";
const NO_SIGNING_SERVICE: &str =
    "An AWS_IAM HTTP data source states the signing service name its requests are signed for, \
     which is not configured for the data source";
const NOT_AN_ENDPOINT: &str =
    "An HTTP data source is reached at a URL carrying a scheme and a host, which is not what is \
     addressed by";
const A_REQUEST_IS_AN_OBJECT: &str =
    "An HTTP data source's request is a JSON object stating the method, the resource path and \
     the parameters of one request, which is not what was returned:";
const PARAMS_ARE_AN_OBJECT: &str =
    "The parameters of an HTTP request are a JSON object, which is not what is carried at";
const PAIRS_ARE_AN_OBJECT: &str =
    "The query parameters and the headers of an HTTP request are each a JSON object, which is \
     not what is carried at";
const A_VALUE_IS_TEXT: &str =
    "A query parameter and a header of an HTTP request each carry text, which is not what is \
     carried at";
const A_FIELD_IS_A_HEADER: &str =
    "A header of an HTTP request names an HTTP field and the text it carries, which is not what \
     is carried at";
const ONE_OF_THE_METHODS: &str = "An HTTP request is made with one of the methods";
const NOT_A_REQUEST: &str = "This HTTP request could not be built:";
const NOT_SIGNED: &str = "This HTTP request could not be signed:";
const NOT_SENT: &str = "This HTTP request was not sent:";
const NOTHING_SERVES_THE_ENDPOINT: &str =
    "No endpoint answered this HTTP request, so the data source produced no result.";
const ANSWER_TOO_LARGE: &str =
    "The answer to this HTTP request is larger than one result carries, which is";

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the request a step sends is the one its request object DESCRIBES — the method, the
    /// resource path under the endpoint's own path, and the query the request states — so the
    /// URL it is sent to is the endpoint resolved against that path rather than a concatenation
    /// of the two.
    ///
    /// The rows differ along what the resolution generalises over: the endpoint's own path
    /// (none, one, one with a trailing slash), the resource path (the root, a nested one, one
    /// carrying its own query string, one stated without a leading slash and none at all) and
    /// the query parameters the request states.
    #[test]
    fn the_request_is_sent_to_the_endpoint_resolved_against_its_resource_path() {
        let rows: &[(&str, &str, Json, &str)] = &[
            (
                "the root path of a bare endpoint",
                "https://sqs.us-east-1.amazonaws.com",
                json!({"method": "POST", "resourcePath": "/"}),
                "https://sqs.us-east-1.amazonaws.com/",
            ),
            (
                "no resource path at all",
                "https://sqs.us-east-1.amazonaws.com",
                json!({"method": "POST"}),
                "https://sqs.us-east-1.amazonaws.com/",
            ),
            (
                "a nested resource path",
                "https://orders.example.com",
                json!({"method": "GET", "resourcePath": "/v1/orders/A-1"}),
                "https://orders.example.com/v1/orders/A-1",
            ),
            (
                "a resource path under the endpoint's own path",
                "https://orders.example.com/stage",
                json!({"method": "GET", "resourcePath": "/orders"}),
                "https://orders.example.com/stage/orders",
            ),
            (
                "an endpoint path carrying a trailing slash",
                "https://orders.example.com/stage/",
                json!({"method": "GET", "resourcePath": "/orders"}),
                "https://orders.example.com/stage/orders",
            ),
            (
                "a resource path stated without a leading slash",
                "https://orders.example.com",
                json!({"method": "GET", "resourcePath": "orders"}),
                "https://orders.example.com/orders",
            ),
            (
                "a resource path carrying its own query string",
                "https://orders.example.com",
                json!({"method": "GET", "resourcePath": "/orders?status=open"}),
                "https://orders.example.com/orders?status=open",
            ),
            (
                "the query parameters the request states",
                "https://orders.example.com",
                json!({
                    "method": "GET",
                    "resourcePath": "/orders",
                    "params": {"query": {"status": "open", "limit": 5}},
                }),
                "https://orders.example.com/orders?status=open&limit=5",
            ),
            (
                "a parameter the request states several values for",
                "https://orders.example.com",
                json!({
                    "method": "GET",
                    "resourcePath": "/orders",
                    "params": {"query": {"id": ["A-1", "A-2"]}},
                }),
                "https://orders.example.com/orders?id=A-1&id=A-2",
            ),
            (
                "a resource path's own query string beside the parameters stated",
                "https://orders.example.com",
                json!({
                    "method": "GET",
                    "resourcePath": "/orders?status=open",
                    "params": {"query": {"limit": "5"}},
                }),
                "https://orders.example.com/orders?status=open&limit=5",
            ),
        ];
        for (case, endpoint, requested, expected) in rows {
            let call = Call::read(requested).expect("a well-formed request object is read");
            let addressed = call
                .addressed(endpoint)
                .expect("an absolute endpoint is resolved");
            assert_eq!(
                &addressed, expected,
                "case {case:?}: expected {expected:?}, got {addressed:?}"
            );
        }
    }

    /// RULE: a request object this data source cannot send is refused, and the refusal locates
    /// the offending value by its member path.
    #[test]
    fn a_request_this_data_source_cannot_send_is_refused() {
        let rows: &[(&str, Json, &str)] = &[
            (
                "a method this endpoint does not make a request with",
                json!({"method": "TRACE", "resourcePath": "/"}),
                "[method]",
            ),
            (
                "no method at all",
                json!({"resourcePath": "/"}),
                "[method]",
            ),
            (
                "parameters that are not an object",
                json!({"method": "GET", "params": "status=open"}),
                "[params]",
            ),
            (
                "query parameters that are not an object",
                json!({"method": "GET", "params": {"query": ["status=open"]}}),
                "[params][query]",
            ),
            (
                "a header carrying an object",
                json!({"method": "GET", "params": {"headers": {"x-order": {"id": "A-1"}}}}),
                "[params][headers][x-order]",
            ),
            (
                "a request that is not an object at all",
                json!("GET /orders"),
                "",
            ),
        ];
        for (case, requested, located) in rows {
            let refused = Call::read(requested).expect_err("a request this endpoint cannot send");
            assert!(
                refused.contains(located),
                "case {case:?}: expected the refusal to locate {located:?}, got {refused:?}"
            );
        }
    }

    /// RULE: the fields a request object states are the ones the request carries, except the
    /// ones the TRANSPORT owns — the endpoint's own host, the signature and the body's framing —
    /// which a request object does not set.
    ///
    /// The rows differ along the field: one the request states and the transport does not own,
    /// the three the transport does own (in either case), and the body the request carries.
    #[test]
    fn the_fields_the_transport_owns_are_not_a_request_objects_to_set() {
        let requested = json!({
            "method": "POST",
            "resourcePath": "/",
            "params": {
                "headers": {
                    "x-amz-target": "AmazonSQS.SendMessage",
                    "content-type": "application/x-amz-json-1.0",
                    "Host": "elsewhere.example.com",
                    "authorization": "AWS4-HMAC-SHA256 Credential=stolen/20260101/us-east-1/sqs/aws4_request",
                    "Content-Length": "7",
                },
                "body": {"QueueUrl": "https://sqs.us-east-1.amazonaws.com/000000000000/orders"},
            },
        });
        let call = Call::read(&requested).expect("a well-formed request object is read");
        let addressed = call
            .addressed("https://sqs.us-east-1.amazonaws.com")
            .expect("an absolute endpoint is resolved");
        let request = call
            .http_request(&addressed)
            .expect("the request is built from what the object states");
        let rows: &[(&str, &str, Option<&str>)] = &[
            (
                "a field the request states",
                "x-amz-target",
                Some("AmazonSQS.SendMessage"),
            ),
            (
                "another field the request states",
                "content-type",
                Some("application/x-amz-json-1.0"),
            ),
            (
                "the host, which the endpoint decides",
                "host",
                Some("sqs.us-east-1.amazonaws.com"),
            ),
            (
                "the signature, which the authorization configuration decides",
                "authorization",
                None,
            ),
            ("the framing of the body", "content-length", None),
        ];
        for (case, field, expected) in rows {
            let carried = request
                .headers()
                .get(*field)
                .and_then(|value| value.to_str().ok());
            assert_eq!(
                carried, *expected,
                "case {case:?}: expected {field} to carry {expected:?}, got {carried:?}"
            );
        }
        let body = String::from_utf8_lossy(request.body()).to_string();
        assert!(
            body.contains("\"QueueUrl\""),
            "case {:?}: expected the body the request states, got {body:?}",
            "the body the request carries"
        );
    }

    /// RULE: only a data source whose authorization configuration states `AWS_IAM` signs its
    /// outbound requests.
    #[test]
    fn only_an_aws_iam_data_source_signs_its_requests() {
        for (case, declared, signs) in [
            ("the one authorization type the model declares", Some("AWS_IAM"), true),
            ("no authorization configuration at all", None, false),
            ("a value that is not that type", Some("API_KEY"), false),
            ("the type in another case", Some("aws_iam"), false),
        ] {
            let source = Configured {
                name: "orders",
                endpoint: Some("https://orders.example.com"),
                authorization_type: declared,
                signing_region: None,
                signing_service_name: Some("execute-api"),
                service_role_arn: None,
            };
            assert_eq!(
                signs_its_requests(&source),
                signs,
                "case {case:?}: expected signs={signs}"
            );
        }
    }
}
