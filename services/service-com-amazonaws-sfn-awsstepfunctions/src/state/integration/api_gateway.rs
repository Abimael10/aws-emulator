// SPDX-License-Identifier: CC0-1.0

//! The optimized Amazon API Gateway integration (`arn:aws:states:::apigateway:invoke`):
//! the `Task` state whose unit of work is one HTTP request to a DEPLOYED API STAGE.
//!
//! Every other optimized integration names an OPERATION of another service's API, whose
//! input the state's parameters are ([`super`]). This one names no operation at all: its
//! parameters address a stage of an api and describe the request to make on it —
//! `ApiEndpoint` (the hostname of the api's invoke endpoint), `Stage`, `Path`, `Method`,
//! `Headers`, `QueryParameters`, `RequestBody`, `AllowNullValues` and `AuthType` — and
//! what the stage ANSWERS is the state's result, under the four members an endpoint's
//! answer is reported by (`StatusCode`, `StatusText`, `Headers`, `ResponseBody`). So the
//! reading of those parameters and the naming of the answer live here, while the request
//! itself, the transport it goes over and the rendering of the answer are the HTTP Task's
//! ([`crate::state::http_task`]): two integrations, one request-making and one reporting.
//!
//! ## Where the request goes, and what it carries
//!
//! To the stage, in process. The request is handed to the in-process transport every
//! emulated service reaches another by, so whichever service of this emulator CLAIMS the
//! `ApiEndpoint` host serves it — API Gateway's own `execute-api` data plane, for a host
//! naming an api this emulator holds — and the status, headers and body the task reports
//! are that stage's own. Nothing is resolved, nothing is dialled, and an `ApiEndpoint`
//! naming an endpoint nothing here serves reaches no endpoint at all.
//!
//! `AuthType` decides what AUTHORIZATION the request carries, out of the three values
//! that integration documents:
//!
//! * `NO_AUTH` (the value a task that names none gets) sends the request UNSIGNED, so the
//!   stage decides it for an anonymous caller — which is the api's own resource policy;
//! * `IAM_ROLE` and `RESOURCE_POLICY` both sign it as the state machine's execution role
//!   ([`crate::state::execution_role_session`]), under the signing name the invoke
//!   endpoint authenticates ([`EXECUTE_API_SIGNING_NAME`]), so the stage decides it for
//!   that role's own session exactly as it decides any other signed invoke — against the
//!   policies in force on the role AND the api's document.
//!
//! Nothing here judges the call: WHO may invoke a deployed method is the api's own
//! decision, taken by the plane that serves the invoke from the account's own records, so
//! a refusal the task reports is that plane's own answer rather than a verdict this
//! module reached.
//!
//! ## What the task reports
//!
//! A 2xx answer is the task's RESULT. Any other status is its FAILURE, named in the
//! `ApiGateway` error family: the exception class the answer itself names
//! ([`ERROR_TYPE_HEADER`], which is how an `execute-api` endpoint reports the class of a
//! refusal — `ApiGateway.AccessDeniedException` for a call the api's documents do not
//! allow), and the STATUS for an answer that names no class, which is what a response
//! passed through from the method's own backend carries
//! (`ApiGateway.404`). The cause is the whole answer as the result reports it — so a
//! `Catch` on the error name routes the failure and the response that caused it is
//! readable in the cause — except for an authorization refusal, whose cause is this
//! service's own sentence about the action rather than the endpoint's own body about the
//! principal and the method ARN ([`task_cause`]).

use super::*;

/// The one action this integration has.
const INVOKE_ACTION: &str = "invoke";

/// The signing name an invoke of a deployed stage is signed under: the `execute-api`
/// service AWS publishes the invoke endpoint and the `execute-api:Invoke` action under,
/// which is NOT the control plane's own name ([`API_GATEWAY_API`]'s `signing_name`) and is
/// what the plane serving the invoke authenticates a signature against.
const EXECUTE_API_SIGNING_NAME: &str = "execute-api";

/// The IAM action an invoke of a deployed method asks for, which the `execute-api` service
/// publishes it under and which a refusal of such a call names ([`task_cause`]).
const INVOKE_IAM_ACTION: &str = "execute-api:Invoke";

/// The scheme an api's invoke endpoint is addressed under.
const INVOKE_SCHEME: &str = "https";

/// Amazon API Gateway, for the one call this integration makes to it.
///
/// Kept out of [`APIS`] — the table the optimized family looks a service's OPERATIONS up
/// in — because this integration calls no operation of that API: its request is the one
/// the state's parameters describe, so none of the per-operation wire facts an entry
/// carries is read for it ([`Call::endpoint`]). What IS read is the name Step Functions
/// writes for the service in a failed task's error name
/// ([`Api::error_prefix`]).
///
/// `services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/
/// com-amazonaws-apigateway-backplanecontrolservice.json`
/// (`smithy_query(query="service_info")`): restJson1, sigv4Name `apigateway`,
/// endpointPrefix `apigateway`, arnNamespace `apigateway` ([`APIGATEWAY_NAMESPACE`], the
/// token the optimized family names this integration by), sdkId `API Gateway`,
/// cloudFormationName `ApiGateway`.
pub(super) static API_GATEWAY_API: Api = Api {
    namespace: APIGATEWAY_NAMESPACE,
    action_prefix: None,
    signing_name: APIGATEWAY_NAMESPACE,
    endpoint_prefix: APIGATEWAY_NAMESPACE,
    // No operation of this service is called through this integration, so no operation's
    // bindings are stated for one.
    wire: Wire::Rest(&[]),
    sdk_id: "API Gateway",
    // The name a failed `apigateway:invoke` task is reported under: the service's name
    // without the space its SDK id carries, which is also the name AWS publishes the
    // service under in a template (`cloudFormationName` `ApiGateway`).
    error_prefix: "ApiGateway",
    // The AWS SDK names the service `ApiGateway`, which its own `ApiGateway` client
    // carries.
    sdk_error_prefix: "ApiGateway",
    // The client class `AmazonApiGatewayClient`, whose base exception is
    // `AmazonApiGatewayException`; the model's service shape is the service's internal
    // name (`BackplaneControlService`), which the SDK's own names do not carry.
    sdk_client: "AmazonApiGateway",
    sdk_exception: "AmazonApiGatewayException",
    // The `execute-api` endpoint answers a call it does not serve, and one its documents
    // do not allow, as a Forbidden.
    access_denied_status: TARGET_FORBIDDEN_STATUS,
    access_denied_code: COMMON_ACCESS_DENIED_CODE,
    lowercases_refused_action: false,
    // This integration's parameters are PascalCase (`ApiEndpoint`, `Method`, `Stage`),
    // and they are read here rather than put on the wire as an operation's members.
    member_case: MemberCase::Pascal,
    query_compatible: false,
};

/// The member that names the api's invoke endpoint: "the hostname of an API Gateway URL",
/// so it addresses the stage and carries no path of its own.
const API_ENDPOINT_MEMBER: &str = "ApiEndpoint";

/// The member that names the STAGE the request is made on, which is the first segment of
/// a deployed api's invoke path.
const STAGE_MEMBER: &str = "Stage";

/// The member that names the resource path under that stage.
const PATH_MEMBER: &str = "Path";

/// The member that names the request's HTTP method.
const METHOD_MEMBER: &str = "Method";

/// The member carrying the headers the request sends, as a map of header name to one
/// value or a list of values.
const HEADERS_MEMBER: &str = "Headers";

/// The member carrying the request's query-string parameters, in the same two forms.
const QUERY_PARAMETERS_MEMBER: &str = "QueryParameters";

/// The member carrying the request's body, as the document the state built.
const REQUEST_BODY_MEMBER: &str = "RequestBody";

/// The member that asks for the NULL members of that body to be sent as they stand; a
/// task that names none sends a body with its null members dropped.
const ALLOW_NULL_VALUES_MEMBER: &str = "AllowNullValues";

/// The member that names the authorization the request is made under, and its three
/// values: no authorization at all, the state machine's role, and the api's own resource
/// policy — the last two both signed as that role.
const AUTH_TYPE_MEMBER: &str = "AuthType";
/// See [`AUTH_TYPE_MEMBER`].
const NO_AUTH: &str = "NO_AUTH";
/// See [`AUTH_TYPE_MEMBER`].
const IAM_ROLE: &str = "IAM_ROLE";
/// See [`AUTH_TYPE_MEMBER`].
const RESOURCE_POLICY: &str = "RESOURCE_POLICY";

/// The media type the body a `RequestBody` carries travels under: the document the state
/// built, sent as the JSON it is.
const JSON_MEDIA_TYPE: &str = "application/json";

/// The call `arn:aws:states:::apigateway:invoke` names, or why no call is made for what
/// the `Resource` wrote after the `apigateway` namespace.
///
/// `action` is the action the `Resource` named with the integration PATTERN split off it,
/// and `pattern` is that pattern: this integration has one action and serves the
/// request-response and callback (`.waitForTaskToken`) forms of it, so anything else
/// names work no request is made for rather than a request assembled out of it. `shown`
/// is the `Resource` as a cause may echo it ([`crate::state::asl::echoed`]).
pub(super) fn read_invoke(
    shown: &str,
    action: &str,
    pattern: Option<&str>,
) -> Result<Call, String> {
    if action != INVOKE_ACTION {
        return Err(format!(
            "'{shown}' names '{}' of this service's API Gateway integration, which performs \
             only its '{INVOKE_ACTION}' action, so nothing was run for it",
            crate::state::asl::echoed(action)
        ));
    }
    let callback = match pattern {
        None => false,
        Some(CALLBACK_PATTERN) => true,
        Some(pattern) => {
            return Err(format!(
                "'{shown}' asks for the '{}' pattern of this service's API Gateway \
                 integration, which it does not serve, so nothing was run for it",
                crate::state::asl::echoed(pattern)
            ))
        }
    };
    Ok(Call {
        api: &API_GATEWAY_API,
        // An optimized integration, which is what names a failure of it.
        family: Family::Optimized,
        operation: INVOKE_ACTION.to_string(),
        // Every one of these belongs to a call on a service's OPERATION, and this call is
        // on none: there is no REST binding to build a request with, no job to park at,
        // no member to convert, no SDK response to wrap and no [`TARGET_RESOURCES`] entry
        // naming a resource of another service the call acts on — WHO may invoke the
        // stage is the api's own decision, taken where the invoke is served.
        rest: None,
        job: None,
        callback,
        parsed_output: false,
        json_text: &[],
        json_text_at: &[],
        form_maps: &[],
        sdk_envelope: false,
        epoch_millis: &[],
        targets: None,
        invokes: None,
        endpoint: Some(EndpointTask::ApiGateway),
        // No operation's input is built for this call at all, so there is no payload
        // member to put on the wire.
        payload_document: false,
    })
}

/// Make the request on the stage `request` addresses and report what that stage answered.
///
/// `signing` is the session of the state machine's execution role, which is the identity
/// an `IAM_ROLE` or `RESOURCE_POLICY` invoke is made as; a walk that was given none signs
/// with this emulator's own credential, exactly as every other integration call does
/// ([`super::dispatch`]).
pub(super) async fn invoke(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    request: &Value,
    signing: Option<&crate::state::execution_role_session::RoleSession>,
) -> Answer {
    let asked = Asked::read(request)?;
    let mut built = asked.wired()?;
    if asked.auth.is_signed() {
        let signed_as = signer(callback, signing).await?;
        super::dispatching::sign(&mut built, EXECUTE_API_SIGNING_NAME, region, &signed_as)
            .map_err(|error| {
                crate::state::http_task::unsent(format!(
                    "the request this task would have made to '{}' could not be signed: \
                     {error}",
                    crate::state::asl::echoed(&asked.host)
                ))
            })?;
    }
    let answered = crate::state::http_task::dispatch(callback, built).await?;
    let reported = crate::state::http_task::reported(&answered);
    if (200..300).contains(&answered.status) {
        return Ok(reported);
    }
    // The stage answered that it will not do the work: the task fails under the name and
    // the cause that answer gives it.
    let class = error_shape(answered.header(ERROR_TYPE_HEADER), None);
    Err(Failed {
        error: task_error(&answered, class.as_deref()),
        cause: task_cause(class.as_deref(), &reported),
    })
}

/// The cause a failed invoke carries: the answer as the result reports it, and the
/// service's own NOT-AUTHORIZED sentence for the one class whose answer is about the
/// CALLER rather than about the work.
///
/// Every answer that IS the work's outcome is reported whole, so a `Catch` path can read
/// the status, the headers and the body the stage answered. An AUTHORIZATION refusal is
/// not that: it carries, in its own body, the principal the `execute-api` endpoint refused
/// and the method ARN it refused them on, and neither reaches the definition. What the
/// task reports for it is the ACTION an invoke asks for ([`INVOKE_IAM_ACTION`], the one
/// action the `execute-api` service publishes for invoking a deployed method) and no
/// resource at all — which is the whole of that sentence, since nothing else in it comes
/// from the request, the answer or any stored value.
///
/// RECORDED BEHAVIOUR, which is the only reason that class is worded here rather than
/// relayed: a `Task` state invoking a stage of an api whose RESOURCE POLICY denies the
/// caller fails with `ApiGateway.AccessDeniedException` and exactly that cause, while the
/// same api's `execute-api` endpoint answers the IAM-shaped sentence naming the principal
/// and the method ARN in its `403` body — which is what this emulator's own API Gateway
/// data plane answers too, and `core/router/tests/all/sfn_api_gateway_invoke_task.rs`
/// asserts that it stays out of the cause. So a definition's `Catch` path reads the same
/// sentence whichever api, stage, method or role the denied call named.
fn task_cause(class: Option<&str>, reported: &Value) -> String {
    match class == Some(API_GATEWAY_API.access_denied_code()) {
        true => format!("Not authorized to perform: {INVOKE_IAM_ACTION} on the resource"),
        false => reported.to_string(),
    }
}

/// The error name a task whose stage answered outside 2xx reports: the `ApiGateway`
/// family, closed by the exception CLASS the answer named itself by, and by the STATUS it
/// answered for an answer that named no class.
///
/// An `execute-api` endpoint names the class of its own refusals in
/// [`ERROR_TYPE_HEADER`] — the header a restJson1 client resolves an error shape from —
/// so a call the api's documents do not allow reaches a definition as
/// `ApiGateway.AccessDeniedException` and is matched by a `Catch` written for it. A
/// response the method's own backend produced names no AWS error class at all, and is
/// named by the status it carries.
fn task_error(
    answered: &crate::state::http_task::Answered,
    class: Option<&str>,
) -> String {
    match class {
        Some(shape) => API_GATEWAY_API.task_error(Family::Optimized, shape),
        None => format!("{}.{}", API_GATEWAY_API.error_prefix, answered.status),
    }
}

/// WHAT the request is signed with: the execution role's own session where the walk was
/// given one, and this emulator's own credential otherwise.
///
/// The same choice [`super::dispatch`] makes for every other integration call, reached
/// through the same two items rather than through a second reading of it.
async fn signer(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    signing: Option<&crate::state::execution_role_session::RoleSession>,
) -> Result<super::dispatching::Signer, Failed> {
    match signing {
        Some(session) => Ok(super::dispatching::Signer {
            access_key_id: session.access_key_id.clone(),
            secret_access_key: session.secret_access_key.clone(),
            session_token: session.session_token.clone(),
        }),
        None => {
            let (access_key_id, secret_access_key) = super::dispatching::credentials(callback)
                .await
                .map_err(|error| {
                    crate::state::http_task::unsent(format!(
                        "this task's request could not be signed, because this emulator's own \
                         credential could not be read ({error}), so nothing was run for it"
                    ))
                })?;
            Ok(super::dispatching::Signer {
                access_key_id,
                secret_access_key,
                session_token: None,
            })
        }
    }
}

/// What one API Gateway invoke asks for, as its parameters say it.
#[derive(Debug, Clone)]
struct Asked {
    /// The host the request is addressed to, which is the api's own invoke endpoint.
    host: String,
    /// The absolute URL of the stage resource the request is made on: that host, the
    /// stage and the path under it.
    url: String,
    /// Its HTTP method.
    method: ::http::Method,
    /// The headers the task asks for, in the order read; one entry per value, so a header
    /// the task repeats is sent once per value.
    headers: Vec<(String, String)>,
    /// Its query-string parameters, the same way.
    query: Vec<(String, String)>,
    /// The body it sends, as the bytes of the document the state built; `None` for a task
    /// that names none.
    body: Option<Vec<u8>>,
    /// The authorization the request carries.
    auth: Auth,
}

/// The authorization one invoke is made under, out of the three values `AuthType`
/// documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Auth {
    /// `NO_AUTH`: the request carries no credential, so the stage decides it for an
    /// anonymous caller.
    None,
    /// `IAM_ROLE`: the request is signed as the state machine's execution role.
    Role,
    /// `RESOURCE_POLICY`: signed as that same role, so the api's own document decides the
    /// call against the principal it speaks for.
    ResourcePolicy,
}

impl Auth {
    /// The authorization `named` asks for, the documented default for a task that names
    /// none, and a refusal for a value that is none of the three — an authorization this
    /// emulator does not guess at.
    fn read(named: Option<&str>) -> Result<Self, Failed> {
        match named {
            None | Some(NO_AUTH) => Ok(Self::None),
            Some(IAM_ROLE) => Ok(Self::Role),
            Some(RESOURCE_POLICY) => Ok(Self::ResourcePolicy),
            Some(other) => Err(crate::state::http_task::unsent(format!(
                "this task's '{AUTH_TYPE_MEMBER}' is '{}', which is none of \
                 '{NO_AUTH}', '{IAM_ROLE}' and '{RESOURCE_POLICY}', so no request was made \
                 for it",
                crate::state::asl::echoed(other)
            ))),
        }
    }

    /// Whether a request made under this authorization carries a SIGNATURE.
    fn is_signed(self) -> bool {
        match self {
            Self::None => false,
            Self::Role | Self::ResourcePolicy => true,
        }
    }
}

impl Asked {
    /// What `request` asks for, or why no request is made for it.
    ///
    /// Each refusal says what the parameters named and what about it cannot be sent, and
    /// never claims the request happened.
    fn read(request: &Value) -> Result<Self, Failed> {
        let endpoint = crate::state::http_task::required_text(request, API_ENDPOINT_MEMBER)?;
        let Some(host) = addressed_host(&endpoint) else {
            return Err(crate::state::http_task::unsent(format!(
                "this task's '{API_ENDPOINT_MEMBER}' is '{}', which is not the hostname of an \
                 API Gateway endpoint, so no request was made for it",
                crate::state::asl::echoed(&endpoint)
            )));
        };
        let named = crate::state::http_task::required_text(request, METHOD_MEMBER)?;
        let method = ::http::Method::from_bytes(named.as_bytes()).map_err(|_| {
            crate::state::http_task::unsent(format!(
                "this task's '{METHOD_MEMBER}' is '{}', which is not an HTTP method, so no \
                 request was made for it",
                crate::state::asl::echoed(&named)
            ))
        })?;
        let stage = optional_text(request, STAGE_MEMBER)?;
        let path = optional_text(request, PATH_MEMBER)?;
        let auth = Auth::read(optional_text(request, AUTH_TYPE_MEMBER)?.as_deref())?;
        Ok(Self {
            url: format!(
                "{INVOKE_SCHEME}://{host}{}",
                stage_path(stage.as_deref(), path.as_deref())
            ),
            host,
            method,
            headers: crate::state::http_task::pairs(request, HEADERS_MEMBER)?,
            query: crate::state::http_task::pairs(request, QUERY_PARAMETERS_MEMBER)?,
            body: body_of(request),
            auth,
        })
    }

    /// The request this task makes.
    ///
    /// Its own framing is written first — the host it is addressed to, the media type of
    /// its body — and the task's own `Headers` then REPLACE what that framing wrote under
    /// a name they mention, so a task that names its own media type gets ONE such header
    /// rather than two, while a header it repeats keeps every value. A header name is
    /// matched without regard to case, as an HTTP field name is (RFC 9110); the merge is
    /// the HTTP Task's own ([`crate::state::http_task`]), so the two integrations cannot
    /// merge one differently.
    fn wired(&self) -> Result<::http::Request<Vec<u8>>, Failed> {
        let mut headers: Vec<(String, String)> = vec![(
            ::http::header::HOST.as_str().to_string(),
            self.host.clone(),
        )];
        if self.body.is_some() {
            headers.push((
                ::http::header::CONTENT_TYPE.as_str().to_string(),
                JSON_MEDIA_TYPE.to_string(),
            ));
        }
        crate::state::http_task::replacing(&mut headers, &self.headers);
        let mut builder = ::http::Request::builder()
            .method(self.method.clone())
            .uri(crate::state::http_task::with_query(&self.url, &self.query));
        for (name, value) in &headers {
            builder = builder.header(name, value);
        }
        builder
            .body(self.body.clone().unwrap_or_default())
            .map_err(|error| {
                crate::state::http_task::unsent(format!(
                    "the request this task would have made could not be built: {error}"
                ))
            })
    }
}

/// The HOST `endpoint` names: the authority of a value written as a URL, read through the
/// tree's one reading of an absolute-form request target, and the value itself where it is
/// the hostname the member documents and nothing else.
///
/// `None` for a value that names no host at all — one carrying a path, or nothing.
fn addressed_host(endpoint: &str) -> Option<String> {
    if let Some(authority) =
        ::service_common::data_plane_addressing::addressed_host(endpoint, None)
    {
        return Some(authority);
    }
    let authority = endpoint.parse::<::http::uri::Authority>().ok()?;
    Some(authority.to_string())
}

/// The path one invoke is made on: the `Stage` and then the `Path`, each as one or more
/// segments of it, and the endpoint's root for a task that names neither.
///
/// Both members are segments of the invoke path a deployed api publishes
/// (`https://{api-id}.execute-api.{region}.amazonaws.com/{stage}/{path}`), so they are
/// joined as segments: the separators each one was written with do not double up, and a
/// member that is empty contributes nothing.
fn stage_path(stage: Option<&str>, path: Option<&str>) -> String {
    let segments: Vec<&str> = [stage, path]
        .into_iter()
        .flatten()
        .map(|part| part.trim_matches('/'))
        .filter(|part| !part.is_empty())
        .collect();
    format!("/{}", segments.join("/"))
}

/// The text `document` carries under `member` for a member that may be left out, or
/// `None` where it names none; a member written as something other than text leaves no
/// request to make.
fn optional_text(document: &Value, member: &str) -> Result<Option<String>, Failed> {
    match document.get(member) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(crate::state::http_task::unsent(format!(
            "this task's '{member}' is not a string, so there is no request to make and \
             nothing was run for it"
        ))),
    }
}

/// The body the task sends: the document its `RequestBody` carries, as the JSON text of
/// that document; `None` for a task that names none.
///
/// A member of it holding NULL is dropped unless the task's `AllowNullValues` asks for one
/// to be sent as it stands, which is what that member selects — so a payload template that
/// resolved a member to nothing sends a body without it rather than one the endpoint has
/// to read a null out of. The removal reaches every member of the document, at every
/// depth, because a null nested inside it is as absent as one at its top level.
fn body_of(request: &Value) -> Option<Vec<u8>> {
    let body = request
        .get(REQUEST_BODY_MEMBER)
        .filter(|body| !body.is_null())?;
    let allow_null = request
        .get(ALLOW_NULL_VALUES_MEMBER)
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let body = match allow_null {
        true => body.clone(),
        false => without_nulls(body),
    };
    Some(body.to_string().into_bytes())
}

/// `value` with every NULL member of every object in it dropped.
fn without_nulls(value: &Value) -> Value {
    match value {
        Value::Object(members) => Value::Object(
            members
                .iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(name, value)| (name.clone(), without_nulls(value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(without_nulls).collect()),
        value => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: `arn:aws:states:::apigateway:invoke` names this integration, in its
    /// request-response and callback forms; anything else written after the namespace
    /// names work no request is made for.
    ///
    /// The rows differ along what the `Resource` wrote: the action alone, each pattern
    /// after it, another action, and an action spelled another way.
    #[test]
    fn the_resource_names_the_invoke_action_and_its_served_patterns() {
        /// (case, the action, the pattern, whether a call is made and whether it waits
        /// for a token)
        type Row<'a> = (&'a str, &'a str, Option<&'a str>, Option<bool>);
        let rows: &[Row<'_>] = &[
            ("the request-response form", INVOKE_ACTION, None, Some(false)),
            (
                "the callback form",
                INVOKE_ACTION,
                Some(CALLBACK_PATTERN),
                Some(true),
            ),
            (
                "the run-a-job pattern, which this integration does not serve",
                INVOKE_ACTION,
                Some(RUN_A_JOB_PATTERN),
                None,
            ),
            ("another action of the service", "createRestApi", None, None),
            ("the action in another case", "Invoke", None, None),
            ("no action at all", "", None, None),
        ];
        for (case, action, pattern, expected) in rows {
            let read = read_invoke("resource", action, *pattern)
                .ok()
                .map(|call| call.callback);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: the OPTIMIZED family names this integration (by the service's ARN namespace),
    /// and the AWS SDK family's token for the same service names that service's CONTROL
    /// plane instead — so a `Resource` of that family is no invoke of a stage.
    ///
    /// Read through [`super::read`], the one reading of a `Resource`, because which family
    /// a token belongs to is that reading's own step.
    #[test]
    fn only_the_optimized_family_names_this_integration() {
        /// (case, the `Resource`, the endpoint work it names)
        type Row<'a> = (&'a str, &'a str, Option<EndpointTask>);
        let rows: &[Row<'_>] = &[
            (
                "the optimized integration",
                "arn:aws:states:::apigateway:invoke",
                Some(EndpointTask::ApiGateway),
            ),
            (
                "its callback form",
                "arn:aws:states:::apigateway:invoke.waitForTaskToken",
                Some(EndpointTask::ApiGateway),
            ),
            (
                "the AWS SDK family's token for the same service",
                "arn:aws:states:::aws-sdk:apigateway:getRestApis",
                None,
            ),
            (
                "the AWS SDK family naming the invoke action",
                "arn:aws:states:::aws-sdk:apigateway:invoke",
                None,
            ),
        ];
        for (case, resource, expected) in rows {
            let read = super::read(resource).ok().map(|call| call.endpoint);
            assert_eq!(
                read.flatten(),
                *expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: the request is made on the stage and the path the parameters name, as
    /// segments of the invoke path a deployed api publishes.
    ///
    /// The rows differ along the form each member takes: both named, each on its own,
    /// neither, a path of several segments, and the separators a definition may write
    /// around either.
    #[test]
    fn the_request_path_is_the_stage_and_the_path_under_it() {
        /// (case, the stage, the path, the path the request is made on)
        type Row<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a str);
        let rows: &[Row<'_>] = &[
            ("a stage and a path", Some("prod"), Some("orders"), "/prod/orders"),
            (
                "a path of several segments",
                Some("prod"),
                Some("orders/42/items"),
                "/prod/orders/42/items",
            ),
            ("a stage alone", Some("prod"), None, "/prod"),
            ("a path alone", None, Some("orders"), "/orders"),
            ("neither, which is the endpoint's root", None, None, "/"),
            (
                "separators written around both",
                Some("/prod/"),
                Some("/orders/"),
                "/prod/orders",
            ),
            ("members that are empty", Some(""), Some(""), "/"),
        ];
        for (case, stage, path, expected) in rows {
            let written = stage_path(*stage, *path);
            assert_eq!(
                written, *expected,
                "case {case:?}: expected {expected:?}, got {written:?}"
            );
        }
    }

    /// RULE: the `ApiEndpoint` is the HOSTNAME of an api's invoke endpoint, and a value
    /// written as a URL names its host in its authority.
    ///
    /// The rows differ along the form the value takes: the documented hostname, one on an
    /// explicit port, a value written as a URL (with and without a path), and values that
    /// name no host at all.
    #[test]
    fn the_api_endpoint_names_one_host() {
        /// (case, the value, the host it addresses)
        type Row<'a> = (&'a str, &'a str, Option<&'a str>);
        let rows: &[Row<'_>] = &[
            (
                "the documented form: the api's own invoke hostname",
                "abc1230000.execute-api.us-east-1.amazonaws.com",
                Some("abc1230000.execute-api.us-east-1.amazonaws.com"),
            ),
            (
                "a host on an explicit port",
                "127.0.0.1:9297",
                Some("127.0.0.1:9297"),
            ),
            (
                "a value written as a URL",
                "https://abc1230000.execute-api.eu-west-1.amazonaws.com",
                Some("abc1230000.execute-api.eu-west-1.amazonaws.com"),
            ),
            (
                "a URL carrying a path, whose path is no part of the address",
                "https://abc1230000.execute-api.eu-west-1.amazonaws.com/prod",
                Some("abc1230000.execute-api.eu-west-1.amazonaws.com"),
            ),
            ("a hostname with a path after it", "host/prod", None),
            ("nothing at all", "", None),
        ];
        for (case, endpoint, expected) in rows {
            let read = addressed_host(endpoint);
            assert_eq!(
                read.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: the body is the document `RequestBody` carries, with its null members
    /// dropped unless `AllowNullValues` asks for them.
    ///
    /// The rows differ along what the body holds (nothing, a null member, a null nested
    /// in an object and in an array) and what the task said about nulls.
    #[test]
    fn the_body_carries_the_documents_null_members_only_when_asked() {
        /// (case, the parameters, the body sent)
        type Row<'a> = (&'a str, Value, Option<&'a str>);
        let rows: &[Row<'_>] = &[
            ("no body at all", ::serde_json::json!({}), None),
            (
                "a body written as null",
                ::serde_json::json!({ "RequestBody": Value::Null }),
                None,
            ),
            (
                "a document with no null member",
                ::serde_json::json!({ "RequestBody": { "a": 1 } }),
                Some(r#"{"a":1}"#),
            ),
            (
                "a null member, dropped by default",
                ::serde_json::json!({ "RequestBody": { "a": 1, "b": Value::Null } }),
                Some(r#"{"a":1}"#),
            ),
            (
                "a null member the task asks to send",
                ::serde_json::json!({
                    "RequestBody": { "a": 1, "b": Value::Null },
                    "AllowNullValues": true,
                }),
                Some(r#"{"a":1,"b":null}"#),
            ),
            (
                "a null nested inside the document",
                ::serde_json::json!({
                    "RequestBody": { "a": { "b": Value::Null, "c": 2 }, "d": [{ "e": Value::Null }] },
                }),
                Some(r#"{"a":{"c":2},"d":[{}]}"#),
            ),
            (
                "a body that is not a document at all",
                ::serde_json::json!({ "RequestBody": "plain text" }),
                Some(r#""plain text""#),
            ),
        ];
        for (case, parameters, expected) in rows {
            let body = body_of(parameters).map(|bytes| String::from_utf8_lossy(&bytes).to_string());
            assert_eq!(
                body.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {body:?}"
            );
        }
    }

    /// RULE: the request carries a signature for the two `AuthType`s that name an
    /// identity, none for the one that names none, and a value naming no authorization at
    /// all leaves no request to make.
    #[test]
    fn the_auth_type_decides_whether_the_request_is_signed() {
        /// (case, the value, whether the request is signed; `None` where none is made)
        type Row<'a> = (&'a str, Option<&'a str>, Option<bool>);
        let rows: &[Row<'_>] = &[
            ("no value, which is the documented default", None, Some(false)),
            ("no authorization", Some(NO_AUTH), Some(false)),
            ("the state machine's role", Some(IAM_ROLE), Some(true)),
            ("the api's resource policy", Some(RESOURCE_POLICY), Some(true)),
            ("a value naming none of the three", Some("SIGV4"), None),
            ("the empty string", Some(""), None),
        ];
        for (case, named, expected) in rows {
            let read = Auth::read(*named).ok().map(Auth::is_signed);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: a non-2xx answer fails the task in the `ApiGateway` family, under the
    /// exception class the answer named itself by and under its STATUS where it named
    /// none.
    ///
    /// And the CAUSE is the answer itself, except for the authorization refusal, which
    /// carries this service's own sentence about the action instead of the endpoint's body
    /// about the principal and the method.
    ///
    /// The rows differ along what the answer carried: the class an `execute-api` refusal
    /// names, another class, a class carrying the shape-id decoration the protocol allows,
    /// and no class at all at two different statuses.
    #[test]
    fn a_refused_invoke_is_named_in_the_api_gateway_family() {
        /// (case, the status, the error class the answer named, the body it carried, the
        /// task's error name, its cause)
        type Row<'a> = (&'a str, u16, Option<&'a str>, &'a str, &'a str, &'a str);
        let rows: &[Row<'_>] = &[
            (
                "the class an api's own documents refuse an invoke with",
                403,
                Some("AccessDeniedException"),
                "{\"Message\":\"User: arn:aws:sts::0:assumed-role/r/s is not authorized to \
                 perform: execute-api:Invoke on resource: arn:aws:execute-api:x\"}",
                "ApiGateway.AccessDeniedException",
                "Not authorized to perform: execute-api:Invoke on the resource",
            ),
            (
                "the class an endpoint serving nothing of the api answers",
                403,
                Some("ForbiddenException"),
                "{\"message\":\"Forbidden\"}",
                "ApiGateway.ForbiddenException",
                "{\"Headers\":{\"x-amzn-errortype\":[\"ForbiddenException\"]},\
                 \"ResponseBody\":{\"message\":\"Forbidden\"},\"StatusCode\":403,\
                 \"StatusText\":\"Forbidden\"}",
            ),
            (
                "a class carrying the shape-id decoration restJson1 allows",
                403,
                Some("AccessDeniedException:http://internal.amazon.com/coral/"),
                "",
                "ApiGateway.AccessDeniedException",
                "Not authorized to perform: execute-api:Invoke on the resource",
            ),
            (
                "a backend answer naming no class",
                404,
                None,
                "{\"served\":\"not-found\"}",
                "ApiGateway.404",
                "{\"ResponseBody\":{\"served\":\"not-found\"},\"StatusCode\":404,\
                 \"StatusText\":\"Not Found\"}",
            ),
            (
                "a backend fault naming no class",
                502,
                None,
                "",
                "ApiGateway.502",
                "{\"StatusCode\":502,\"StatusText\":\"Bad Gateway\"}",
            ),
        ];
        for (case, status, named, body, expected, expected_cause) in rows {
            let answered = crate::state::http_task::Answered {
                status: *status,
                headers: named
                    .map(|named| vec![(ERROR_TYPE_HEADER.to_string(), named.to_string())])
                    .unwrap_or_default(),
                payload: body.as_bytes().to_vec(),
            };
            let class = error_shape(answered.header(ERROR_TYPE_HEADER), None);
            let name = task_error(&answered, class.as_deref());
            assert_eq!(
                name, *expected,
                "case {case:?}: expected {expected:?}, got {name:?}"
            );
            let reported = crate::state::http_task::reported(&answered);
            let cause = task_cause(class.as_deref(), &reported);
            assert_eq!(
                cause, *expected_cause,
                "case {case:?}: expected the cause {expected_cause:?}, got {cause:?}"
            );
        }
    }

    /// RULE: the request is addressed to the host the `ApiEndpoint` names, on the stage
    /// path, with the task's own headers replacing the framing's and its query parameters
    /// on the target.
    #[test]
    fn the_request_addresses_the_stage_the_parameters_name() {
        let asked = Asked::read(&::serde_json::json!({
            "ApiEndpoint": "abc1230000.execute-api.us-east-1.amazonaws.com",
            "Method": "POST",
            "Stage": "prod",
            "Path": "orders",
            "QueryParameters": { "dryRun": "true" },
            "Headers": { "content-type": "application/xml", "x-trace": ["a", "b"] },
            "RequestBody": { "item": "42" },
            "AuthType": IAM_ROLE,
        }))
        .expect("the parameters describe a request");
        let built = asked.wired().expect("the request is well formed");
        assert_eq!(
            built.uri().to_string(),
            "https://abc1230000.execute-api.us-east-1.amazonaws.com/prod/orders?dryRun=true",
            "expected the stage's own path and the task's query, got {:?}",
            built.uri()
        );
        assert_eq!(
            built.method().as_str(),
            "POST",
            "expected the method the parameters name, got {:?}",
            built.method()
        );
        let carried: Vec<String> = built
            .headers()
            .get_all(::http::header::CONTENT_TYPE)
            .iter()
            .map(|value| value.to_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            carried,
            vec!["application/xml".to_string()],
            "expected the task's own media type to replace the framing's, got {carried:?}"
        );
        let traced: Vec<String> = built
            .headers()
            .get_all("x-trace")
            .iter()
            .map(|value| value.to_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            traced,
            vec!["a".to_string(), "b".to_string()],
            "expected both values of the repeated header, got {traced:?}"
        );
        assert_eq!(
            built.headers().get(::http::header::HOST).map(|host| host
                .to_str()
                .unwrap_or_default()
                .to_string()),
            Some("abc1230000.execute-api.us-east-1.amazonaws.com".to_string()),
            "expected the host the ApiEndpoint names, got {:?}",
            built.headers().get(::http::header::HOST)
        );
        assert_eq!(
            String::from_utf8_lossy(built.body()),
            r#"{"item":"42"}"#,
            "expected the document the RequestBody carries, got {:?}",
            built.body()
        );
        assert!(
            asked.auth.is_signed(),
            "expected an IAM_ROLE invoke to be signed, got {:?}",
            asked.auth
        );
    }

    /// RULE: parameters that describe no request leave no request made, under the name
    /// the interpreter raises its own failures with.
    ///
    /// The rows differ along which parameter is missing or unusable.
    #[test]
    fn parameters_that_describe_no_request_make_none() {
        /// (case, the parameters)
        type Row<'a> = (&'a str, Value);
        let rows: &[Row<'_>] = &[
            ("no endpoint at all", ::serde_json::json!({ "Method": "GET" })),
            (
                "an endpoint that is no hostname",
                ::serde_json::json!({ "ApiEndpoint": "host/with/a/path", "Method": "GET" }),
            ),
            (
                "no method",
                ::serde_json::json!({ "ApiEndpoint": "host.example.com" }),
            ),
            (
                "a method that is not one",
                ::serde_json::json!({ "ApiEndpoint": "host.example.com", "Method": "G E T" }),
            ),
            (
                "a stage that is not text",
                ::serde_json::json!({
                    "ApiEndpoint": "host.example.com",
                    "Method": "GET",
                    "Stage": 7,
                }),
            ),
            (
                "headers that are not a map",
                ::serde_json::json!({
                    "ApiEndpoint": "host.example.com",
                    "Method": "GET",
                    "Headers": "x-a: 1",
                }),
            ),
            (
                "an authorization naming none of the three",
                ::serde_json::json!({
                    "ApiEndpoint": "host.example.com",
                    "Method": "GET",
                    "AuthType": "BASIC",
                }),
            ),
        ];
        for (case, parameters) in rows {
            let read = Asked::read(parameters);
            let failure = read.err().map(|failed| failed.error);
            assert_eq!(
                failure.as_deref(),
                Some(crate::state::asl::RUNTIME_ERROR),
                "case {case:?}: expected no request to be made, got {failure:?}"
            );
        }
    }
}
