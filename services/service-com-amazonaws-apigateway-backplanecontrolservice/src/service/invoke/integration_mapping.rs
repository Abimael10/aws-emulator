// SPDX-License-Identifier: CC0-1.0

//! What an integration's own `requestParameters` map onto the integration request, and the
//! performing of one integration against that mapping.
//!
//! Split out of [`super`] so each file of that module groups one concern and none of them
//! grows past what can be read and reviewed at once. The code is the parent's, moved
//! unchanged: the mapping is the one `Integration$requestParameters` describes, and the
//! performing is the one seam both the `execute-api` data plane and `TestInvokeMethod` reach
//! an integration through.

use super::*;

/// What an integration's `requestParameters` map onto the integration request, by the
/// location each parameter travels in.
#[derive(Debug, Default)]
pub(super) struct MappedRequest {
    /// The headers the integration request carries, in the order the mappings were read.
    pub(super) headers: Vec<(String, String)>,
    /// The PATH parameters, by name: what the `uri`'s own `{name}` placeholders are
    /// addressed with.
    pub(super) path: BTreeMap<String, String>,
    /// The query-string pairs the integration request carries, in the order read.
    pub(super) querystring: Vec<(String, String)>,
}

/// Where `key` says a parameter of the INTEGRATION request travels and under what name, or
/// `None` for a key that names no integration request parameter at all.
///
/// `Integration$requestParameters`' key is "an integration request parameter name", written
/// `integration.request.{location}.{name}` over the three locations a request parameter
/// travels in — `querystring`, `path` or `header`, the same three
/// `Method$requestParameters` names.
pub(super) fn integration_request_parameter(key: &str) -> Option<(ParameterIn, &str)> {
    [
        (INTEGRATION_REQUEST_HEADER_PREFIX, ParameterIn::Header),
        (INTEGRATION_REQUEST_PATH_PREFIX, ParameterIn::Path),
        (INTEGRATION_REQUEST_QUERY_PREFIX, ParameterIn::Query),
    ]
    .into_iter()
    .find_map(|(prefix, location)| {
        key.strip_prefix(prefix)
            .filter(|name| !name.is_empty())
            .map(|name| (location, name))
    })
}

/// `uri` with each `{name}` placeholder replaced by the value `path` gives that name.
///
/// `Integration$uri` is the address the backend is called at, and a path parameter of the
/// integration request is what one of its segments is: the `{name}` the `uri` carries is
/// the parameter `integration.request.path.{name}` maps a value onto, which is how one
/// integration addresses the path the caller asked for — the greedy `{proxy}` of a catch-all
/// resource among them. A placeholder no mapping names is left in place, so the caller can
/// tell an address it cannot resolve from one it can.
///
/// ONE left-to-right pass over the `uri` itself, so a value that has been substituted is
/// never scanned again: the values come from the request (a header, a query parameter, a
/// bound path segment), so rewriting the whole string once per name would let one request's
/// value carry another name's placeholder and make the result the PRODUCT of the values'
/// sizes rather than their sum. It is the same no-rescan contract
/// [`resolved_references`] states for a stage variable, for the same reason.
pub(super) fn expanded_uri(uri: &str, path: &BTreeMap<String, String>) -> String {
    let mut expanded = String::with_capacity(uri.len());
    let mut rest = uri;
    while let Some(open) = rest.find('{') {
        let (before, from_open) = rest.split_at(open);
        expanded.push_str(before);
        // A `{` that closes nowhere is text of the address, not a placeholder.
        let Some(close) = from_open.find('}') else {
            expanded.push_str(from_open);
            return expanded;
        };
        let name = from_open.get(1..close).unwrap_or_default();
        match path.get(name) {
            Some(value) => expanded.push_str(value),
            // A placeholder no mapping names travels on as written.
            None => expanded.push_str(from_open.get(..=close).unwrap_or_default()),
        }
        rest = from_open.get(close + 1..).unwrap_or_default();
    }
    expanded.push_str(rest);
    expanded
}

/// The URI the onward request is addressed at: the integration's `uri` (already addressed,
/// see [`expanded_uri`]), carrying the query parameters the integration MAPS onto the
/// integration request and the CALLER's own query string as well as any the `uri` itself
/// names.
///
/// A proxy integration passes the request through, and a request's query string is part of
/// it: a backend method that declares a query parameter must see the one the caller sent.
/// The `uri`'s own pairs come first and the integration's mapped ones follow — both are the
/// configuration, and a caller cannot displace them — and the caller's come last; a caller
/// that sent none and an integration that maps none leave the `uri` untouched.
///
/// What does NOT travel is the credential a PRESIGNED caller carries in the query string
/// ([`SIGNED_QUERY_PARAMETERS`]), for the reason [`HEADERS_NOT_PASSED_THROUGH`] gives for
/// the header form of the same credential: a signature over the address the caller used
/// authenticates nothing at the address the request is passed on to.
pub(super) fn onward_uri(uri: &str, request_uri: &str, mapped: &[(String, String)]) -> String {
    let mut pairs: Vec<String> = mapped
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    pairs.extend(
        query_of(request_uri)
            .split('&')
            .filter(|pair| !pair.is_empty())
            .filter(|pair| {
                let name = pair.split('=').next().unwrap_or(pair).to_ascii_lowercase();
                !SIGNED_QUERY_PARAMETERS.contains(&name.as_str())
            })
            .map(str::to_string),
    );
    if pairs.is_empty() {
        return uri.to_string();
    }
    let sent = pairs.join("&");
    let separator = if query_of(uri).is_empty() { '?' } else { '&' };
    format!("{uri}{separator}{sent}")
}

/// What one `requestParameters` entry maps onto the integration request.
#[derive(Debug)]
pub(super) enum MappedParameter {
    /// The value to carry onward.
    Value(String),
    /// The entry names a method request parameter this emulator reads and the request
    /// carries none under that name, so the mapping maps nothing at all.
    SourceAbsent,
    /// The entry is written in a form this emulator does not read, so what it would carry
    /// is unknown.
    NotRead,
}

/// What the `requestParameters` value `value` maps onto the integration request.
///
/// The member documents the two forms: "a method request parameter value or static value
/// that must be enclosed within single quotes". The static one is the literal between the
/// quotes; a method request parameter "must match the pattern of
/// `method.request.{location}.{name}`, where `location` is `querystring`, `path`, or
/// `header`", and each of the three is read off this call: its headers, its query string and
/// the PATH PARAMETERS the matched resource template bound (`path_parameters`), which is
/// what lets a catch-all resource's greedy parameter address the backend.
/// The query string is read through `service_common`'s one definition of what a query
/// parameter's value is, so this reads a pair the way the rest of the emulator does; it is
/// the FIRST value under that name, which is the one a single-valued mapping expression
/// names ([`METHOD_REQUEST_MULTI_QUERY_PREFIX`] is the spelling that names them all).
pub(super) fn mapped_request_parameter(
    value: &str,
    req: &RtRequest,
    path_parameters: &BTreeMap<String, String>,
) -> MappedParameter {
    if let Some(literal) = static_mapping_literal(value) {
        return MappedParameter::Value(literal.to_string());
    }
    if let Some(name) = value.strip_prefix(METHOD_REQUEST_HEADER_PREFIX) {
        return match req.headers().get(name) {
            Some(sent) => MappedParameter::Value(sent.to_string()),
            None => MappedParameter::SourceAbsent,
        };
    }
    if let Some(name) = value.strip_prefix(METHOD_REQUEST_QUERY_PREFIX) {
        return match ::service_common::query_parameter(query_of(req.uri()), name) {
            Some(sent) => MappedParameter::Value(sent.to_string()),
            None => MappedParameter::SourceAbsent,
        };
    }
    if let Some(name) = value.strip_prefix(METHOD_REQUEST_PATH_PREFIX) {
        return match path_parameters.get(name) {
            Some(bound) => MappedParameter::Value(bound.clone()),
            None => MappedParameter::SourceAbsent,
        };
    }
    MappedParameter::NotRead
}

/// The payload of a request as the bytes it is passed on with.
///
/// A body this emulator cannot read as bytes — one that was never buffered — passes nothing
/// on, which is what a request with no payload passes on too: every request this data plane
/// is handed arrives buffered.
pub(super) fn request_payload(req: &RtRequest) -> Vec<u8> {
    req.body().bytes().unwrap_or_default().to_vec()
}

/// Why the private connection `integration` reaches its endpoint through cannot carry this
/// call, or `None` when it names no private connection or names one that can.
///
/// `connectionType` `VPC_LINK` is "for private connections between API Gateway and a network
/// load balancer in a VPC" and `connectionId` names the link that connection runs over, so
/// the link is this service's OWN record and is read while the store is held. A link this
/// account does not hold, and one that has not finished provisioning
/// ([`crate::state::VPC_LINK_AVAILABLE`]), carries no traffic — so the integration cannot be
/// performed over it, whatever its `uri` names.
pub(super) fn unusable_vpc_link(
    store: &crate::state::ComAmazonawsApigatewayBackplanecontrolserviceState,
    integration: &GetIntegrationOutput,
) -> Option<String> {
    let connection_type = integration
        .connection_type
        .as_deref()
        .unwrap_or(DEFAULT_CONNECTION_TYPE);
    if connection_type != VPC_LINK_CONNECTION_TYPE {
        return None;
    }
    let Some(connection_id) = integration.connection_id.as_deref() else {
        return Some(format!(
            "the integration's connectionType is {VPC_LINK_CONNECTION_TYPE} and it names no \
             connectionId, so it reaches no network load balancer"
        ));
    };
    // The stage's own variables have already been resolved into the configuration
    // ([`with_stage_variables`]), so a `connectionId` written as a stage-variable
    // reference names the link that variable's value names. A value still carrying `${`
    // is an expression this emulator does not evaluate, so it names no link at all.
    if connection_id.contains("${") {
        return Some(format!(
            "the integration's connectionId is {connection_id}, which is an expression this \
             emulator does not resolve to a VPC link"
        ));
    }
    match store.vpc_links.get(connection_id) {
        None => Some(format!(
            "the integration connects through the VPC link {connection_id}, which this \
             account holds no record of"
        )),
        Some(link) if link.status() != crate::state::VPC_LINK_AVAILABLE => Some(format!(
            "the VPC link {connection_id} this integration connects through is {status}, so \
             it carries no traffic",
            status = link.status()
        )),
        Some(_) => None,
    }
}

/// What performing a method's integration produced: the integration's own ANSWER, and —
/// where the backend's answer was itself a configuration error API Gateway could not use —
/// the sentence the execution log of the request states that error in.
///
/// The second half exists because such an answer is not a refusal: the call WAS performed and
/// the caller is answered from what came back, so nothing in that response says what was
/// wrong with it, and the request's own trace is where API Gateway says
/// ([`lambda_proxy::MALFORMED_PROXY_RESPONSE`] is the one class of it).
#[derive(Debug)]
pub struct Performed {
    /// The integration response the call was answered from, which the integration's own
    /// responses then map ([`integration_response::mapped`]).
    pub answer: IntegrationAnswer,
    /// The configuration error the backend's answer was, or `None` for an answer API Gateway
    /// could use.
    pub configuration_error: Option<&'static str>,
}

impl Performed {
    /// An answer that is no configuration error, which is what every integration performed
    /// from the api's own records or by making another service's call produces.
    fn of(answer: IntegrationAnswer) -> Self {
        Self {
            answer,
            configuration_error: None,
        }
    }
}

/// Perform the call through the method's integration, through the IN-PROCESS transport
/// `ctx` carries where the integration names another emulated service's action.
///
/// The one entry point both sides of this module's work reach — the data plane above and
/// `TestInvokeMethod`, which reports what this performs — so one stored configuration is
/// performed one way whichever side drives it. An `AWS` integration naming a service call
/// this emulator makes is performed by MAKING that call ([`aws_integration`]), an
/// `AWS_PROXY` integration by INVOKING the function it names ([`lambda_proxy`]); every other
/// integration is performed from the api's own records alone ([`perform_integration`]),
/// which needs no transport and is therefore still reachable on its own.
///
/// `region` is the api's own, which is the endpoint an integration ARN that names no Region
/// of its own is called at.
pub async fn performed_integration(
    ctx: &OperationContext<'_>,
    region: &str,
    method: Option<&MethodEntry>,
    call: &MethodRequest,
) -> Result<Performed, UnservedIntegration> {
    if let Some(method) = method {
        if let Some(served) = lambda_proxy::performed(ctx, region, method, call).await {
            // A proxy integration returns the method response itself, so what it answered is
            // an integration response nothing maps ([`integration_response::passed_answer`]).
            return served.map(|answered| Performed {
                answer: integration_response::passed_answer(answered.response),
                configuration_error: answered.configuration_error,
            });
        }
        if let Some(served) = aws_integration::performed(ctx, region, method, call).await {
            return served.map(Performed::of);
        }
    }
    perform_integration(method, call).map(Performed::of)
}

/// Perform the call through the method's integration, answering the INTEGRATION RESPONSE
/// the API's records produce for it — which the integration's own responses then map to the
/// method response, once per request ([`integration_response::mapped`]).
///
/// `MOCK` is the one type that needs no backend, and it is performed in full from those
/// records: the integration REQUEST template this call selects IS what the integration
/// returns, and the `statusCode` that template carries names the integration response the
/// call maps to. A call no template maps is passed through or refused as
/// `passthroughBehavior` says.
///
/// Every other type names a backend whose answer is not this api's records to produce: an
/// `HTTP` or `HTTP_PROXY` naming an endpoint of this emulator is served by carrying its
/// integration request to it ([`onward_request`], which [`serve_resolved`] reaches before
/// this), an `AWS` integration
/// naming an action this emulator serves is served by MAKING that call
/// ([`aws_integration`]), an `AWS_PROXY` is served by INVOKING the function its `uri` names
/// ([`lambda_proxy`]) — both of which [`performed_integration`] reaches before this — and
/// every other one is an integration this call cannot perform rather than one whose answer
/// is invented — see the module docs.
pub fn perform_integration(
    method: Option<&MethodEntry>,
    call: &MethodRequest,
) -> Result<IntegrationAnswer, UnservedIntegration> {
    let Some(integration) = method.and_then(|method| method.integration.as_ref()) else {
        return Err(UnservedIntegration {
            refusal: API_CONFIGURATION_ERROR,
            reason: "the method carries no integration, so there is nothing to call"
                .to_string(),
        });
    };
    let integration_type = integration.r#type.as_deref().unwrap_or_default();
    if integration_type != MOCK_INTEGRATION_TYPE {
        return Err(UnservedIntegration {
            refusal: API_CONFIGURATION_ERROR,
            reason: format!(
                "the {integration_type} integration names a backend this emulator cannot \
                 call: no deployment package runs in process and no outbound connection is \
                 made, so no backend answer exists to map"
            ),
        });
    }

    // The status the call maps to, and what the integration ANSWERED — which a response
    // template reads its payload off. A `MOCK` has no backend, so its answer is the request
    // template's own rendering.
    let (mapped_status, integrated_payload) = match request_mapping(integration, call) {
        // The integration request a `MOCK` answers with is its request template RENDERED
        // against this call: the `statusCode` the rendered text names is the integration
        // response the call maps to, and the rendered text IS what the integration
        // answered, which is what a response template then reads its payload off. A
        // template this emulator does not render is an integration the call cannot be
        // performed through — the same exit its response template takes — rather than a
        // call served with a status and a body nothing produced.
        RequestMapping::Mapped(template) => {
            let body = rendered_mapping_template(template, &call.inputs).map_err(|reason| {
                UnservedIntegration {
                    refusal: API_CONFIGURATION_ERROR,
                    reason: format!(
                        "the {} request template of {} bytes is not one this emulator \
                         renders: {reason}",
                        call.media_type,
                        template.len()
                    ),
                }
            })?;
            (mock_status_code(&body.text), Some(body.text.into_bytes()))
        }
        // Nothing is mapped, so the integration answers the method request's own payload:
        // that is what "passes the method request body through the integration request
        // without transformation" means.
        RequestMapping::PassedThrough => (None, call.inputs.payload.clone()),
        RequestMapping::Unsupported => {
            return Err(UnservedIntegration {
                refusal: UNSUPPORTED_MEDIA_TYPE,
                reason: format!(
                    "no integration request template maps {}, and the integration's \
                     passthroughBehavior does not pass an unmapped body through",
                    call.media_type
                ),
            });
        }
    };
    // A `MOCK` carries no response headers and declares no media type of its own: both are
    // the integration response's own mapping, applied when the answer is mapped.
    Ok(IntegrationAnswer {
        mapping: AnswerMapping::Mock(mapped_status),
        content_type: None,
        payload: integrated_payload,
        headers: BTreeMap::new(),
    })
}
