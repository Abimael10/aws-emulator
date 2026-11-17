// SPDX-License-Identifier: CC0-1.0

//! The two integration types that name an HTTP ENDPOINT — `HTTP` and `HTTP_PROXY` — and the
//! integration request each of them makes of a method request.
//!
//! `IntegrationType` gives them as "`HTTP` for integrating an API method with an HTTP
//! backend" and "`HTTP_PROXY` for integrating with the HTTP proxy integration", and what
//! separates them is the MAPPING, in both directions:
//!
//! * a PROXY integration maps nothing. The caller's own request — its body, its headers and
//!   its query string — is re-addressed at the endpoint the `uri` names and passed on as it
//!   stands, and what came back IS the method response
//!   ([`AnswerMapping::Passed`]);
//! * the STANDARD integration maps both halves from the api's own records. The integration
//!   request carries the body the integration's `requestTemplates` render (or the method
//!   request's own, as `passthroughBehavior` says) and only the parameters its
//!   `requestParameters` map onto it — nothing of the caller's travels unmapped — and the
//!   backend's answer is an ANSWER TO MAP ([`AnswerMapping::Backend`]): the integration
//!   response whose `selectionPattern` matches the status it answered renders the body, and
//!   the method response's declared headers carry what that response's `responseParameters`
//!   map onto them. So a header the backend sent that nothing maps does not reach the caller,
//!   while through a proxy integration it does.
//!
//! Both are performed WITHOUT opening a connection. The address the `uri` names is only ever
//! reached in process: a stage of this same api plane is served by re-entering this module's
//! parent ([`PassedThrough::Request`]), and every other endpoint this emulator publishes over
//! the in-process transport ([`onward_endpoint`]). An address nothing in this emulator claims
//! reaches no backend at all and is answered as the integration this stage cannot perform,
//! with no status, header or body invented for a backend that never ran.

use super::*;

/// Which of the two integration types that name an HTTP endpoint an integration is, and so
/// what its integration request carries and what becomes of the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HttpEndpoint {
    /// `HTTP_PROXY`: the caller's request is passed through as it stands and the backend's
    /// answer is the method response, mapped by nothing.
    Proxy,
    /// `HTTP`: the integration request is the api's own records' to build, and the backend's
    /// answer is mapped by the integration's own responses.
    Mapped,
}

/// Which of the two `integration` is, or `None` for an integration that names no HTTP
/// endpoint at all (`MOCK`, and the two types whose `uri` is an AWS integration ARN).
pub(super) fn http_endpoint(integration: &GetIntegrationOutput) -> Option<HttpEndpoint> {
    let declared = integration.r#type.as_deref()?;
    if declared == HTTP_PROXY_INTEGRATION_TYPE {
        return Some(HttpEndpoint::Proxy);
    }
    if declared == HTTP_INTEGRATION_TYPE {
        return Some(HttpEndpoint::Mapped);
    }
    None
}

/// What an integration naming an HTTP endpoint makes of this request.
#[derive(Debug)]
pub(super) enum PassedThrough {
    /// The integration request to serve against the endpoint the integration's `uri` names,
    /// which is a STAGE OF THIS API PLANE: it is handed back to this plane's own serving.
    ///
    /// Boxed: a whole request is far larger than the refusal beside it, and this value
    /// is returned by value from the resolution below.
    Request(Box<RtRequest>),
    /// The same integration request, addressed at an endpoint of this emulator that is NOT a
    /// stage of this api plane (a Lambda function URL, another service's data plane): it is
    /// carried over the in-process transport instead, and what that endpoint answered is the
    /// backend answer ([`onward_endpoint`]).
    Onward(Box<RtRequest>),
    /// The integration addresses a backend, or carries a mapping, this emulator cannot
    /// perform.
    Unserved(UnservedIntegration),
}

/// The integration request `integration` makes of this request, or `None` when it addresses
/// no HTTP endpoint at all — so the call is performed from the api's own records instead
/// ([`perform_integration`]).
///
/// A `uri` naming an `execute-api` INVOKE ENDPOINT is a stage of THIS api plane, so the
/// request is handed back to [`serve_resolved`] in process — no connection is opened, no name
/// is resolved and no address the caller chose is dialled. The endpoint's own records then
/// decide the answer — which api, which stage, which method, and that api's resource policy —
/// exactly as they would for a client that addressed the endpoint directly, so no backend
/// answer is ever assembled here.
///
/// A `uri` naming ANY OTHER endpoint is carried just the same, over the in-process transport,
/// because this emulator publishes data planes other than this one — a Lambda function URL is
/// an HTTP endpoint an integration names — and the answer is whichever service CLAIMS that
/// endpoint ([`onward_endpoint`]), or the refusal of an integration this stage cannot perform
/// when nothing claims it at all. The two differ only in how the request is carried: one
/// re-enters this plane's own serving, the other crosses the transport, and neither invents a
/// backend answer.
///
/// The endpoint the request is addressed at, and what the api names ITSELF to that endpoint,
/// are the integration's own: the `uri`'s authority is the `Host` the integration request
/// carries and the api's id travels in [`INTEGRATION_API_ID_HEADER`], both of which replace
/// whatever the caller sent under those names.
///
/// WHAT ELSE the integration request carries is where the two types part, as
/// [`HttpEndpoint`] says: a proxy integration carries the caller's own body, headers and
/// query string, and the standard one carries the body its `requestTemplates` render and
/// nothing of the caller's that its `requestParameters` do not map.
///
/// The integration's own `requestParameters` are applied for both: each entry maps "a method
/// request parameter value or static value" onto a header, a path parameter or a query-string
/// parameter of the integration request, the three locations that member names, and the
/// `uri`'s own `{name}` placeholders are addressed from the path ones ([`expanded_uri`]). An
/// entry whose KEY names none of those three locations, one whose VALUE is in neither
/// documented form, and a `uri` placeholder no mapping names are each stated as an
/// integration this call cannot perform rather than silently dropped. A mapping whose SOURCE
/// the request simply does not carry is a different thing from one written in a form that is
/// not read: the parameter is left off, and the request is still made ([`MappedParameter`]).
///
/// `hop` is how many integrations of this emulator's own endpoints this request has already
/// been carried through, and it is checked HERE, before any onward request is built: an
/// integration whose `uri` names the very endpoint it is reached at — or two that name each
/// other — would otherwise re-enter the serving path for ever, so the chain is refused once
/// it reaches [`MAX_PASSED_THROUGH_HOPS`]. The bound sits at the construction site because
/// that is where the loop would be made: nothing can pass a request on without passing it.
pub(super) fn onward_request(
    integration: &GetIntegrationOutput,
    req: &RtRequest,
    hop: u8,
    call: &MethodRequest,
    api_id: &str,
) -> Option<PassedThrough> {
    let carried = http_endpoint(integration)?;
    let path_parameters = &call.inputs.path;
    if hop >= MAX_PASSED_THROUGH_HOPS {
        return Some(PassedThrough::Unserved(UnservedIntegration {
            refusal: API_CONFIGURATION_ERROR,
            reason: format!(
                "the request has been passed through {MAX_PASSED_THROUGH_HOPS} integrations \
                 of this emulator's own endpoints and this one passes it on again, so it is \
                 not followed further"
            ),
        }));
    }
    let uri = integration.uri.as_deref().unwrap_or_default();
    // A `uri` with no authority at all addresses nothing, so nothing is passed on for it.
    // Asked here, before any mapping is read: an integration naming no endpoint is one this
    // plane passes no request through, whatever its parameters map.
    authority_of(uri)?;

    // What `requestParameters` maps onto the integration request, read BEFORE anything is
    // built from it: the PATH parameters decide the address itself, so the mappings are
    // resolved first and the request is addressed from them.
    let mut mapped = MappedRequest::default();
    for (key, value) in integration.request_parameters.iter().flatten() {
        let Some((location, name)) = integration_request_parameter(key) else {
            return Some(PassedThrough::Unserved(UnservedIntegration {
                refusal: API_CONFIGURATION_ERROR,
                reason: format!(
                    "the integration maps {key}, which is not a parameter of the integration \
                     request's header, path or query string, so the mapping is not applied \
                     and the request is not sent"
                ),
            }));
        };
        let value = match mapped_request_parameter(value, req, path_parameters) {
            MappedParameter::Value(mapped) => mapped,
            // The source names a location this emulator reads and the request carries
            // nothing there, so the mapping maps nothing: the backend is called without
            // that parameter, which is what it would be called with on AWS.
            MappedParameter::SourceAbsent => continue,
            MappedParameter::NotRead => {
                return Some(PassedThrough::Unserved(UnservedIntegration {
                    refusal: API_CONFIGURATION_ERROR,
                    reason: format!(
                        "the integration maps {key} from {value}, which is neither a static \
                         value in single quotes nor a method request parameter this emulator \
                         reads"
                    ),
                }));
            }
        };
        match location {
            ParameterIn::Header => mapped.headers.push((name.to_string(), value)),
            ParameterIn::Path => {
                mapped.path.insert(name.to_string(), value);
            }
            ParameterIn::Query => mapped.querystring.push((name.to_string(), value)),
        }
    }

    // The address the backend is called at: the `uri`, with each `{name}` placeholder
    // replaced by the path parameter the integration maps onto that name, and then the
    // query string the call carries onward. A placeholder still left after that names a
    // path parameter the integration maps nothing onto, so the address is unknown.
    let addressed = expanded_uri(uri, &mapped.path);
    if addressed.contains('{') {
        return Some(PassedThrough::Unserved(UnservedIntegration {
            refusal: API_CONFIGURATION_ERROR,
            reason: "the integration's uri carries a placeholder its requestParameters map \
                     no path parameter onto: no request is passed on to a guessed-at address"
                .to_string(),
        }));
    }
    // The endpoint the request is passed on to, read off the ADDRESS it is passed on at and
    // not off the stored spelling: a `{name}` placeholder is filled from a value the CALLER
    // controls ([`mapped_request_parameter`]), so the two can name different endpoints, and
    // what decides this is the address the request is actually made at.
    //
    // An endpoint that is a stage of THIS plane is served by re-entering it, and any other is
    // reached over the in-process transport. A stage of this plane is named in EITHER of the
    // two addressings this plane publishes for one: the `execute-api` endpoint host AWS gives
    // it, and the prefixed path-style address this emulator announces it at
    // ([`announced_address`], `/_aws/execute-api/{api-id}/{stage}/…`) — which is the invoke
    // URL a client of an emulator holds, and so the one an integration naming a stage of this
    // plane carries. Both re-enter this plane's own serving rather than crossing the transport
    // to come back in through the claim: re-entry is what carries `hop`, the count this
    // function is bounded by, and what lets what THIS plane did with the request — the
    // endpoint it delivered it through — travel with it as an argument rather than on the
    // wire. So the question is put below, once the request is built, against the host and
    // path it will be CLAIMED under.
    let Some(host) = authority_of(&addressed) else {
        return Some(PassedThrough::Unserved(UnservedIntegration {
            refusal: API_CONFIGURATION_ERROR,
            reason: "the integration's uri names no endpoint once its path parameters are \
                     applied, so there is no address to pass the request on to"
                .to_string(),
        }));
    };
    // The PAYLOAD the integration request carries, which is where the two types part: a
    // proxy integration passes the caller's own body on as it stands, and the standard one
    // carries what the integration's `requestTemplates` make of it — the same rendering a
    // `MOCK` answers with ([`perform_integration`]), applied to the body the backend is
    // called with.
    let payload = match carried {
        HttpEndpoint::Proxy => request_payload(req),
        HttpEndpoint::Mapped => match mapped_payload(integration, call) {
            Ok(payload) => payload,
            Err(unserved) => return Some(PassedThrough::Unserved(unserved)),
        },
    };
    let declares_media_type = carried == HttpEndpoint::Mapped && !payload.is_empty();
    let mut passed = RtRequest::new(SdkBody::from(payload));
    // The caller's own query string travels only where the request is PASSED THROUGH; a
    // standard integration carries the pairs its `requestParameters` map onto the integration
    // request and no others, which is what mapping a parameter explicitly means. An empty
    // request URI carries no pair of the caller's.
    let caller_query = match carried {
        HttpEndpoint::Proxy => req.uri(),
        HttpEndpoint::Mapped => "",
    };
    if passed
        .set_uri(onward_uri(&addressed, caller_query, &mapped.querystring).as_str())
        .is_err()
    {
        return Some(PassedThrough::Unserved(UnservedIntegration {
            refusal: API_CONFIGURATION_ERROR,
            reason: "the integration's uri is not a URI a request can be addressed at"
                .to_string(),
        }));
    }
    // "Specifies the integration's HTTP method type" (`Integration$httpMethod`): the verb
    // the backend is called with, which is the caller's own where the integration names
    // none — and where it names the `ANY` catch-all, which is a verb no backend is called
    // with but the stand-in for every one of them, exactly as it is on a method.
    let verb = integration
        .http_method
        .as_deref()
        .filter(|declared| !declared.eq_ignore_ascii_case(ANY_METHOD))
        .unwrap_or_else(|| req.method());
    if passed.set_method(verb).is_err() {
        return Some(PassedThrough::Unserved(UnservedIntegration {
            refusal: API_CONFIGURATION_ERROR,
            reason: "the integration's httpMethod is not an HTTP method a request can be \
                     made with"
                .to_string(),
        }));
    }
    // Everything the caller sent travels on where the request is PASSED THROUGH, which is
    // what "the client request passed through as-is" means; a standard integration carries
    // nothing of the caller's unmapped, so the loop is skipped for it and the body's own
    // media type is declared instead.
    if carried == HttpEndpoint::Proxy {
        for (name, value) in req.headers().iter() {
            if HEADERS_NOT_PASSED_THROUGH.contains(&name.to_ascii_lowercase().as_str()) {
                continue;
            }
            // `append`, not `insert`: `iter` yields one entry per value, so a repeated header
            // reaches the backend with every value it arrived with. Through the FALLIBLE form:
            // these are bytes the caller chose, and one the HTTP layer cannot represent must
            // leave the header off rather than abort the request.
            if let Err(refused) = passed
                .headers_mut()
                .try_append(name.to_string(), value.to_string())
            {
                tracing::warn!(
                    header = %name,
                    error = %refused,
                    "the caller's header cannot be carried onward and is left off the request \
                     the integration passes on"
                );
            }
        }
    } else if declares_media_type {
        // The media type the integration request's own body is keyed by: the key of the
        // template that rendered it, which is the type the method request declared
        // ([`request_mapping`] selects a template by it), and the method request's own type
        // where the body was passed through unmapped. A body with no media type is one the
        // backend cannot read.
        if let Err(refused) = passed
            .headers_mut()
            .try_insert(CONTENT_TYPE_HEADER, call.media_type.clone())
        {
            tracing::warn!(
                error = %refused,
                media_type = %call.media_type,
                "the media type of the integration request's body is not one a header can \
                 carry, so the backend is called without it"
            );
        }
    }
    // The endpoint the request is addressed at is the one the `uri` names, so that is the
    // host the integration request carries — the header a client sets from the URL it
    // dials, and what the service claiming that endpoint reads the address off. Set before
    // the integration's own mapped headers, so a mapping onto `host` still has the last
    // word.
    if let Err(refused) = passed.headers_mut().try_insert("host", host.to_string()) {
        tracing::warn!(
            host = %host,
            error = %refused,
            "the endpoint the integration's uri names is not a host a request can carry"
        );
    }
    // The api the request came through, which API Gateway names to the endpoint it passes
    // the request on to ([`INTEGRATION_API_ID_HEADER`]). It is the api's own record rather
    // than anything the caller sent, so it REPLACES a header of that name the caller
    // carried.
    if let Err(refused) = passed
        .headers_mut()
        .try_insert(INTEGRATION_API_ID_HEADER, api_id.to_string())
    {
        tracing::warn!(
            api_id = %api_id,
            error = %refused,
            "the api id is not a value a header can carry, so the integration request does \
             not name the api"
        );
    }
    for (name, value) in mapped.headers {
        // A mapped header REPLACES what the caller sent under that name, which is what a
        // mapping onto it declares; a value the wire cannot carry is stated rather than
        // dropped, because the backend would then be called without it.
        if passed
            .headers_mut()
            .try_insert(name.clone(), value)
            .is_err()
        {
            return Some(PassedThrough::Unserved(UnservedIntegration {
                refusal: API_CONFIGURATION_ERROR,
                reason: format!(
                    "the value the integration maps onto \
                     {INTEGRATION_REQUEST_HEADER_PREFIX}{name} is not one a header can carry"
                ),
            }));
        }
    }
    // WHICH of the two the request is passed on as, decided on the request as it will
    // actually be CLAIMED — after the integration's own mapped headers, which have the last
    // word over the `host` set above. A service reads an onward request's endpoint off that
    // header, so a request whose final host or path names a stage of THIS plane re-enters
    // this plane's own serving, where `hop` travels with it; deciding it on the address
    // before the mappings were applied would let a mapping onto `host` send the request out
    // over the transport and back in through the claim with the count reset, which is a
    // chain nothing then bounds.
    let within_this_plane = {
        let claimed_host = passed.headers().get("host").unwrap_or(host);
        endpoint_labels(claimed_host).is_some()
            || split_execute_api_prefix(path_of(passed.uri())).is_some()
    };
    Some(if within_this_plane {
        PassedThrough::Request(Box::new(passed))
    } else {
        // Passed on over the IN-PROCESS transport, which dispatches it back into this same
        // emulator ([`onward_endpoint`]): no socket is opened, no name is resolved and no
        // address this configuration named is dialled, so the endpoint the `uri` names is
        // reached only if a service of this emulator CLAIMS it.
        PassedThrough::Onward(Box::new(passed))
    })
}

/// The body a STANDARD (non-proxy) integration request carries: what the integration's
/// `requestTemplates` and `passthroughBehavior` make of the method request's own payload.
///
/// `requestTemplates` is "a map of Velocity templates that are applied on the request payload
/// based on the value of the Content-Type header sent by the client", so the template this
/// call selects IS the integration request's body ([`request_mapping`] is the one reading of
/// that selection, shared with the `MOCK` integration that answers the same rendering). A
/// content type no template maps travels on unmapped, or earns the `415` the behaviour
/// documents — "rejects the method request with an HTTP 415 Unsupported Media Type response"
/// — and a template this emulator does not render is an integration this call cannot be
/// performed through rather than a backend called with a body built by guesswork.
fn mapped_payload(
    integration: &GetIntegrationOutput,
    call: &MethodRequest,
) -> Result<Vec<u8>, UnservedIntegration> {
    match request_mapping(integration, call) {
        RequestMapping::Mapped(template) => rendered_mapping_template(template, &call.inputs)
            .map(String::into_bytes)
            .map_err(|reason| UnservedIntegration {
                refusal: API_CONFIGURATION_ERROR,
                reason: format!(
                    "the {} request template of {} bytes is not one this emulator renders: \
                     {reason}",
                    call.media_type,
                    template.len()
                ),
            }),
        RequestMapping::PassedThrough => Ok(call.inputs.payload.clone().unwrap_or_default()),
        RequestMapping::Unsupported => Err(UnservedIntegration {
            refusal: UNSUPPORTED_MEDIA_TYPE,
            reason: format!(
                "no integration request template maps {}, and the integration's \
                 passthroughBehavior does not pass an unmapped body through",
                call.media_type
            ),
        }),
    }
}
