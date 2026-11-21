// SPDX-License-Identifier: CC0-1.0

//! What a WEBSOCKET api's stage ANSWERS: which route serves a message, and what an
//! integration that reaches no backend replies with.
//!
//! A WebSocket api serves two kinds of call over one socket, and the route each is served by
//! is decided here from the configuration the stage's deployment froze:
//!
//! * the CONNECTION REQUEST, the opening handshake itself, served by the `$connect` route
//!   when the api has one ([`admission`]);
//! * each MESSAGE the client then sends, served by the route the api's
//!   `RouteSelectionExpression` selects ([`message_route`]).
//!
//! Everything in this module is a PURE function of the api's records and the bytes the
//! client sent: nothing here touches the socket, the store or the clock, so the rule each
//! answer follows can be read — and tested — without a connection. The BACKEND a route
//! fronts is performed by [`super::integration`], which holds the in-process transport; what
//! is decided here is what an integration answers without one, which is the `MOCK` type's
//! whole rule ([`mock_reply`]) and is also what a reading of the records can say about a
//! connection request before any socket exists ([`super::serve`]).
//!
//! # The rule a `MOCK` integration's reply follows
//!
//! A `MOCK` integration has no backend: "a MOCK integration lets API Gateway return a
//! response without sending the request further to the backend". Its reply is therefore
//! computed from the integration's own records, in the order API Gateway computes it:
//!
//! 1. the integration's `RequestTemplates` entry that keys the call is RENDERED, and the
//!    `statusCode` member of what it renders to is the status the call produced — that is
//!    what a `MOCK` request template is for ("`{"statusCode": 200}`");
//! 2. that status SELECTS one of the integration's `IntegrationResponses` by its
//!    `IntegrationResponseKey`: a key written as a regular expression between slashes
//!    (`/2\d\d/`) is selected when it matches the status, and `$default` is the fallback for
//!    a call no patterned key matches;
//! 3. the selected response's own `ResponseTemplates` entry is RENDERED, and the text it
//!    renders to is the payload the caller reads back.
//!
//! A call that selects NO integration response has no reply at all — API Gateway has no
//! mapping to answer from — which is a fault of the api's configuration rather than of the
//! caller: the connection request fails with `500`, and a message is answered with the
//! gateway's own `Internal server error` frame.
//!
//! The CONNECTION REQUEST is one of the calls such an integration answers: `MOCK` is the
//! integration type "supported only for WebSocket APIs", and a `$connect` route bound to one
//! is answered from that integration's own records, so the status its reply carries admits or
//! refuses the handshake exactly as a backend's reply would ([`admission`]).

use std::collections::BTreeMap;

use crate::service::metrics::CallOutcome;
use crate::state::integration_response_key::{key_matches_status, DEFAULT_SELECTION_KEY};
use crate::state::{Api, DeployedConfiguration, Integration, IntegrationResponse, Route};

/// The `RouteKey` of the route that serves the CONNECTION REQUEST: AWS documents `$connect`
/// as the route "API Gateway invokes when a client connects to your API". Re-exported from
/// the one place the control plane holds the reserved keys
/// (`crate::state::route_request_parameters`), so the grammar a route key is written in and
/// the key this dispatcher matches on cannot drift.
pub use crate::state::route_request_parameters::CONNECT_ROUTE_KEY;

/// The `RouteKey` of the route that serves the close: "API Gateway invokes [it] when a
/// client disconnects from your API" — re-exported from `crate::state::route_key`.
pub use crate::state::DISCONNECT_ROUTE_KEY;

/// The `RouteKey` that serves every message no other route's key matches: "`$default`, which
/// API Gateway invokes if the route selection expression cannot be evaluated or if no
/// matching route is found" — re-exported from `crate::state::route_key`.
pub use crate::state::DEFAULT_ROUTE_KEY;

/// The media type a mapping template is keyed by when it is keyed by one at all — the key
/// API Gateway's own console writes for a WebSocket api's templates, and the one a message
/// carrying JSON selects.
pub const JSON_TEMPLATE_KEY: &str = "application/json";

/// The `IntegrationType` whose reply is computed from the integration's own records; see the
/// module docs.
pub const MOCK_INTEGRATION_TYPE: &str = "MOCK";

/// What a route's integration replied with: the status the call produced and the payload the
/// selected integration response rendered, if it rendered one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// The status the call produced — for a `MOCK`, the `statusCode` its rendered request
    /// template named — or `None` when the call produced none.
    pub status_code: Option<String>,
    /// The `IntegrationResponseKey` of the response the status selected.
    pub selected_key: String,
    /// The payload the selected response's `ResponseTemplates` rendered, or `None` when it
    /// carries no template that keys the call (a reply with no payload).
    pub payload: Option<String>,
}

/// Why a route's integration produced no reply at all.
///
/// Each variant answers the two questions the serving stage's own series ask about a call that
/// failed: WHOSE failure it was ([`Unserved::metered_as`]), and whether the backend ANSWERED
/// the invocation at all ([`Unserved::answered`]), which is what there is a latency to report
/// for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unserved {
    /// The route names no integration of this api, or names none at all, so there is
    /// nothing to call.
    NoIntegration,
    /// No `IntegrationResponse` of the integration is selected by the status the call
    /// produced, so API Gateway has no mapping to answer from.
    NoSelectableResponse,
    /// The integration is of a type this emulator does not perform here, carries a template
    /// it does not render, or names no backend to address, so NO invocation was made; the
    /// string says which, for the log and for the developer.
    Unperformable(String),
    /// The invocation was made and the backend never answered it: it was not delivered, it
    /// was refused before anything ran, or it had not answered when the integration's own
    /// timeout elapsed.
    Undelivered(String),
    /// The function the integration fronts RAN and ended in an unhandled error, so its answer
    /// is an error document rather than a reply to map.
    FunctionError(String),
    /// The backend ANSWERED and its answer is one the gateway cannot read as the reply a
    /// proxy integration owes.
    UnreadableReply(String),
    /// The call carries a content type this integration maps no request template for, and its
    /// `PassthroughBehavior` does not pass an unmapped call through, so the backend was never
    /// reached: "NEVER rejects unmapped content types" (`Integration$PassthroughBehavior`).
    /// The client reads the gateway's own `Internal server error` frame for it, the answer
    /// every call whose route produced no reply gets on this data plane.
    UnsupportedMediaType(String),
}

impl Unserved {
    /// Which of the serving stage's error series a call this did not serve increments — the
    /// ONE class that names whose failure it was.
    ///
    /// The ROUTE's execution is what did not reach a successful end when the gateway had
    /// nothing to call, could not perform what the route names, or ran a function that ended
    /// in an unhandled error; the INTEGRATION's own class is for a backend that answered the
    /// gateway nothing it could use — one that never answered the invocation, and one whose
    /// answer cannot be read.
    ///
    /// Exactly one class per failing call, which is what [`CallOutcome`] carries.
    pub fn metered_as(&self) -> CallOutcome {
        match self {
            Self::Undelivered(_) | Self::UnreadableReply(_) => CallOutcome::IntegrationError,
            Self::NoIntegration
            | Self::NoSelectableResponse
            | Self::Unperformable(_)
            | Self::UnsupportedMediaType(_)
            | Self::FunctionError(_) => CallOutcome::ExecutionError,
        }
    }

    /// Whether the backend ANSWERED the invocation the gateway made, however unusable that
    /// answer was: a function that ran and raised answered it (with the error document Lambda
    /// reports for an unhandled error), and so did one whose reply the gateway cannot read.
    ///
    /// It is what decides whether the call has a latency to report: `IntegrationLatency` is
    /// the time between the gateway relaying the call to the backend and the backend's answer
    /// reaching it, so a call the backend answered has one and a call that reached no backend —
    /// or whose backend never answered — has none.
    pub fn answered(&self) -> bool {
        matches!(self, Self::FunctionError(_) | Self::UnreadableReply(_))
    }
}

/// The route of the deployed configuration whose `RouteKey` is `route_key`, or `None` when
/// the deployment holds none.
pub fn deployed_route<'d>(
    deployed: &'d DeployedConfiguration,
    route_key: &str,
) -> Option<&'d Route> {
    deployed
        .routes
        .values()
        .find(|route| route.route_key == route_key)
}

/// The integration `route` targets, read from the SAME deployment the route came from, so a
/// stage pinned to an older deployment calls the integration as it stood then.
pub fn targeted_integration<'d>(
    deployed: &'d DeployedConfiguration,
    route: &Route,
) -> Option<&'d Integration> {
    let integration_id = route
        .target
        .as_deref()
        .and_then(crate::state::target_integration_id)?;
    deployed.integrations.get(integration_id)
}

/// What the CONNECTION REQUEST is answered with as far as the configuration the stage
/// deployed DECIDES IT, WITHOUT reaching a backend — see [`admission`].
///
/// Three answers rather than two, because the reading of a configuration and the answer to a
/// handshake are not the same question: a route that fronts a BACKEND is neither admitted nor
/// refused by the records, and a caller that reads one as a refusal gives the gateway's
/// configuration-fault status to a configuration API Gateway serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// ADMITTED by the configuration itself: the deployment holds no `$connect` route, so
    /// there is nothing to refuse the connection.
    Admitted,
    /// REFUSED by the configuration itself: the `$connect` route's integration can produce no
    /// reply for the connection request at all — it targets no integration of this api, or the
    /// `MOCK` it targets selects no integration response to answer from — so the handshake
    /// fails whatever a backend would have said. The string says which, for this emulator's
    /// own log.
    Refused(String),
    /// DECIDED by the integration's own records: the `$connect` route is bound to a `MOCK`,
    /// which "lets API Gateway return a response without sending the request further to the
    /// backend", so the reply the records compute IS the connection request's answer and the
    /// status it carries is outside `2xx` — the handshake is refused with THAT status and the
    /// payload that reply rendered, the same answer the api's own socket gives it.
    ///
    /// A `MOCK` whose reply is a `2xx` is [`Self::Admitted`]: there is nothing left to refuse
    /// the connection.
    RepliedWith(Reply),
    /// UNDECIDED by the configuration: the `$connect` route fronts a BACKEND, and the
    /// connection is admitted or refused by the status that backend answers, which no reading
    /// of the records can give. The caller that holds an in-process transport obtains it
    /// ([`super::integration::perform`]).
    BackendDecides,
}

/// What the configuration the stage deployed says about the CONNECTION REQUEST, without
/// reaching a backend.
///
/// The handshake completes only when the route that serves the connection request produced a
/// SUCCESSFUL reply: the `$connect` route's integration is called before the socket exists,
/// and its reply is what admits the connection. So:
///
/// * an api with NO `$connect` route admits the connection — there is no route to refuse it;
/// * a `$connect` route whose integration replies with a `2xx` status admits it
///   ([`is_success_status`], applied to the reply that integration gave);
/// * a `$connect` route whose integration produces no reply, or a reply outside `2xx`, fails
///   the handshake.
///
/// A `MOCK` integration serves the connection request from the integration's OWN RECORDS:
/// `MOCK` is the type "for integrating the route or method request with API Gateway as a
/// 'loopback' endpoint without invoking any backend. Supported only for WebSocket APIs"
/// (`CreateIntegration$IntegrationType`), so the reply it computes ([`mock_reply`]) IS this
/// call's reply, and the status that reply carries decides the handshake exactly as a
/// backend's would: a `2xx` admits the connection ([`Admission::Admitted`]) and anything else
/// refuses it with that status and the payload the selected response rendered
/// ([`Admission::RepliedWith`]). A `MOCK` that selects no integration response produces no
/// reply at all, which is a fault of the api's configuration — [`Admission::Refused`], the
/// gateway's own `500` — and so is a route that targets no integration: there is nothing to
/// serve the connection request with.
///
/// Every OTHER integration type fronts a backend, and what that backend answered is the whole
/// decision: this reading reports [`Admission::BackendDecides`] rather than a refusal, so the
/// gateway's `500` — the answer a configuration that cannot be served earns — is never given
/// to a configuration whose only unknown is a reply nobody has asked the backend for yet.
///
/// The records are read ALONE here, with no call to render a template against: the status a
/// `MOCK` request template names is the template's own literal (`{"statusCode": 200}`, the
/// documented form), so a template that derived its status from `$context` or
/// `$stageVariables` instead is out of this reading's scope and is decided where the CALL is
/// in hand ([`super::integration::perform`]), which is the path every connection request a
/// socket carries takes.
pub fn admission(deployed: &DeployedConfiguration) -> Admission {
    let Some(route) = deployed_route(deployed, CONNECT_ROUTE_KEY) else {
        return Admission::Admitted;
    };
    let Some(integration) = targeted_integration(deployed, route) else {
        return Admission::Refused(format!(
            "the {CONNECT_ROUTE_KEY} route targets no integration of this api, so nothing \
             serves the connection request"
        ));
    };
    if integration.integration_type != MOCK_INTEGRATION_TYPE {
        return Admission::BackendDecides;
    }
    match mock_reply(integration, None) {
        Ok(reply) if is_success_status(reply.status_code.as_deref()) => Admission::Admitted,
        Ok(reply) => Admission::RepliedWith(reply),
        Err(unserved) => Admission::Refused(format!(
            "the {CONNECT_ROUTE_KEY} route's {MOCK_INTEGRATION_TYPE} integration answers the \
             connection request from its own records and produced no reply: {unserved:?}"
        )),
    }
}

/// Whether a status a reply carries is a SUCCESS — the `2xx` class, which is what admits a
/// connection request.
pub fn is_success_status(status: Option<&str>) -> bool {
    status
        .and_then(|status| status.trim().parse::<u16>().ok())
        .is_some_and(|status| (200..300).contains(&status))
}

/// The route key the api's `RouteSelectionExpression` selects for `body`, or `None` when the
/// expression selects nothing in this message.
///
/// A WEBSOCKET api's expression names a PATH INTO THE MESSAGE BODY
/// (`crate::state::WEBSOCKET_ROUTE_SELECTION_REFERENCE_PREFIX`, the rule every writer of the
/// member is held to), so the key is the value that path reaches: `$request.body.action`
/// selects the value of `action`, and `$request.body.meta.kind` the value of `kind` inside
/// `meta`. A message that is not a JSON object, or that carries no such value, selects
/// nothing — which is the "route selection expression cannot be evaluated" case `$default`
/// exists for.
pub fn selected_route_key(api: &Api, body: &str) -> Option<String> {
    let reference = crate::state::selection_expression_references(&api.route_selection_expression)
        .into_iter()
        .next()?;
    // WHICH part of the call the reference names is the one reading of that set
    // (`crate::state::selection_reference`): the namespace is read case-insensitively, the
    // way the writer of the member reads it, and the PATH exactly as written, because a JSON
    // member name is case-sensitive. A WEBSOCKET api's expression names a path into the BODY
    // (`crate::state::WEBSOCKET_ROUTE_SELECTION_REFERENCE_PREFIX`, the rule every writer of
    // the member is held to), so a reference naming anything else selects nothing.
    let (namespace, path) = crate::state::selection_reference(&reference)?;
    if namespace != crate::state::SelectionNamespace::RequestBody {
        return None;
    }
    let document: ::serde_json::Value = ::serde_json::from_str(body).ok()?;
    // The path is resolved by the service's ONE reader of a body reference
    // (`crate::state::selected_body_value`), so the route a message selects and the template
    // a call selects read the same reference the same way.
    crate::state::selected_body_value(&document, path)
}

/// The route of this deployment that serves the message `body`: the one whose `RouteKey` the
/// api's `RouteSelectionExpression` selected, then the `$default` route, and `None` when the
/// api holds neither — a message it routes nowhere.
///
/// The ONE selection both callers make, so what a reading of the records says a message is
/// routed to and what the socket actually invokes cannot disagree.
pub fn message_route<'d>(
    api: &Api,
    deployed: &'d DeployedConfiguration,
    body: &str,
) -> Option<&'d Route> {
    selected_route_key(api, body)
        .as_deref()
        .and_then(|key| deployed_route(deployed, key))
        .or_else(|| deployed_route(deployed, DEFAULT_ROUTE_KEY))
}

/// Whether `route` is TWO-WAY: "To define a two-way route, create a route response" — a
/// route with none gets no reply sent back to it however its integration replied.
pub fn is_two_way(route: &Route) -> bool {
    !route.route_responses.is_empty()
}

/// The call a mapping template is rendered against, when there is one to render against: a
/// reading of the api's records that has no connection in hand renders none.
///
/// It carries the WHOLE context of the call rather than one member of it: a template reads
/// `$context.<member>` for any member the gateway reports about the call it is rendered for
/// ([`super::event::template_context`] is the one owner of that set), `$stageVariables.<name>`
/// for the serving stage's own variables, and `$input` over the frame's payload — which only a
/// call that CARRIES a frame has, so the body is optional the way a close's is.
#[derive(Debug, Clone, Copy)]
pub struct TemplateCall<'c> {
    context: &'c BTreeMap<String, String>,
    stage_variables: &'c BTreeMap<String, String>,
    body: Option<&'c str>,
}

impl<'c> TemplateCall<'c> {
    /// One call of a connection, as a template reads it: its `$context` values, the serving
    /// stage's variables, and the frame it carries when it carries one.
    pub fn new(
        context: &'c BTreeMap<String, String>,
        stage_variables: &'c BTreeMap<String, String>,
        body: Option<&'c str>,
    ) -> Self {
        Self {
            context,
            stage_variables,
            body,
        }
    }
}

/// The reply a `MOCK` integration computes for this call, from the integration's own
/// records: the rule the module docs state, in the order API Gateway computes it.
pub fn mock_reply(
    integration: &Integration,
    call: Option<TemplateCall<'_>>,
) -> Result<Reply, Unserved> {
    let status = match keyed_template(&integration.request_templates) {
        Some(template) => {
            let rendered = rendered_template(template, call)?;
            status_code_of(&rendered)
        }
        None => None,
    };
    let selected = selected_integration_response(integration, status.as_deref())
        .ok_or(Unserved::NoSelectableResponse)?;
    let payload = match keyed_template(&selected.response_templates) {
        Some(template) => Some(rendered_template(template, call)?),
        None => None,
    };
    Ok(Reply {
        status_code: status,
        selected_key: selected.integration_response_key.clone(),
        payload,
    })
}

/// The template of `templates` that keys this call: the `$default` entry, then the JSON one,
/// then the single entry a map that holds exactly one carries.
///
/// A WebSocket message carries no `Content-Type` of its own, so what keys the call is the
/// map's own fallback key rather than a media type the client named; a map holding one
/// template under some other key is still that integration's one template, so it is the one
/// rendered.
pub(super) fn keyed_template(templates: &BTreeMap<String, String>) -> Option<&str> {
    if let Some(keyed) = templates
        .get(DEFAULT_SELECTION_KEY)
        .or_else(|| templates.get(JSON_TEMPLATE_KEY))
    {
        return Some(keyed.as_str());
    }
    if templates.len() == 1 {
        return templates.values().next().map(String::as_str);
    }
    None
}

/// `template` rendered against this call, or why this emulator does not render it.
///
/// A mapping template is "a map of Velocity templates" (`Integration$RequestTemplates`), and
/// this emulator renders one in ONE place: the renderer the REST data plane of API Gateway
/// uses, which resolves the closed set of references API Gateway documents a template
/// against and refuses a template carrying a DIRECTIVE — a statement that would have to be
/// EXECUTED — rather than passing it through as if it were a literal body
/// (`service_com_amazonaws_apigateway_backplanecontrolservice::service::mapping_template`).
/// What this function owns is the BINDING of that renderer's inputs to a WebSocket call: the
/// frame is the payload `$input.body`, `$input.json(…)` and `$input.path(…)` read, and the
/// `$context` and `$stageVariables` are the call's own ([`TemplateCall`]).
///
/// A template carrying no reference IS the body and renders to itself, which is what a
/// `MOCK` integration's `{"statusCode": 200}` is.
pub(super) fn rendered_template(
    template: &str,
    call: Option<TemplateCall<'_>>,
) -> Result<String, Unserved> {
    let inputs = ::service_com_amazonaws_apigateway_backplanecontrolservice::service::mapping_template::TemplateInputs {
        payload: call.and_then(|call| call.body).map(|body| body.as_bytes().to_vec()),
        context: call.map(|call| call.context.clone()).unwrap_or_default(),
        stage_variables: call.map(|call| call.stage_variables.clone()).unwrap_or_default(),
        ..Default::default()
    };
    ::service_com_amazonaws_apigateway_backplanecontrolservice::service::mapping_template::rendered_without_directives(template, &inputs)
        .map_err(|unrendered| Unserved::Unperformable(unrendered.reason))
}

/// The status a rendered `MOCK` request template names, or `None` when it names none.
///
/// The documented form is a literal object carrying the status the call produces
/// (`{"statusCode": 200}`), so this reads that member off what the template RENDERED TO: a
/// rendering that is not such an object names no status, and the call then selects the
/// integration response that is the fallback rather than one guessed at.
fn status_code_of(rendered: &str) -> Option<String> {
    let value: ::serde_json::Value = ::serde_json::from_str(rendered).ok()?;
    match value.get("statusCode")? {
        ::serde_json::Value::Number(number) => number.as_u64().map(|code| code.to_string()),
        ::serde_json::Value::String(code) => Some(code.clone()),
        _ => None,
    }
}

/// The integration response `status` selects, or `None` when the integration holds none that
/// this call selects.
///
/// A patterned key is tried first, in key order so the choice is stable whatever order the
/// map yields, and `$default` is the fallback — which is also the only key a call that
/// produced NO status can select.
pub(super) fn selected_integration_response<'i>(
    integration: &'i Integration,
    status: Option<&str>,
) -> Option<&'i IntegrationResponse> {
    if let Some(status) = status {
        let matched = integration
            .integration_responses
            .values()
            .filter(|response| response.integration_response_key != DEFAULT_SELECTION_KEY)
            .find(|response| key_matches_status(&response.integration_response_key, status));
        if matched.is_some() {
            return matched;
        }
    }
    integration
        .integration_responses
        .values()
        .find(|response| response.integration_response_key == DEFAULT_SELECTION_KEY)
}

#[cfg(test)]
mod tests {
    //! The pure rules this module owns, over the inputs each generalises over. The behaviour
    //! end to end — a client's handshake, its frames and the replies it reads — is driven
    //! over a real socket in `tests/all/websocket_transport_data_plane.rs`.

    use super::*;

    #[test]
    fn an_integration_response_key_selects_the_statuses_its_pattern_matches() {
        // The rows span what the rule generalises over: each of the patterned forms a key
        // takes, a literal key, the whole-match requirement, and a key no engine builds.
        let cases: &[(&str, &str, bool)] = &[
            (r"/2\d\d/", "200", true),
            (r"/2\d\d/", "299", true),
            (r"/2\d\d/", "404", false),
            (r"/4\d{2}/", "404", true),
            (r"/4\d{2}/", "200", false),
            // The whole status, not a part of it.
            (r"/2\d\d/", "1200", false),
            ("200", "200", true),
            ("200", "201", false),
            ("/(/", "200", false),
            ("//", "200", false),
        ];
        for (key, status, expected) in cases {
            let selected = key_matches_status(key, status);
            assert_eq!(
                selected, *expected,
                "case ({key:?}, {status:?}): expected {expected}, got {selected}"
            );
        }
    }

    #[test]
    fn a_rendered_template_names_the_status_it_carries() {
        // The rows span what the reading generalises over: the documented literal form, the
        // quoted spelling, an object carrying other members too, a body naming no status,
        // and text that is no JSON object at all.
        let cases: &[(&str, Option<&str>)] = &[
            (r#"{"statusCode": 200}"#, Some("200")),
            (r#"{"statusCode": "404"}"#, Some("404")),
            (r#"{"statusCode":201,"body":"x"}"#, Some("201")),
            (r#"{"body":"x"}"#, None),
            ("not json", None),
        ];
        for (rendered, expected) in cases {
            let named = status_code_of(rendered);
            assert_eq!(
                named.as_deref(),
                *expected,
                "case {rendered:?}: expected {expected:?}, got {named:?}"
            );
        }
    }

    #[test]
    fn a_template_renders_its_literal_text_and_the_references_a_call_has_values_for() {
        // The `$context` of the call is the one the gateway reports about it, keyed by the
        // member path a template writes (`super::super::event::template_context`).
        let context = BTreeMap::from([
            ("connectionId".to_string(), "L0SM9cOFyjUCIhA=".to_string()),
            ("routeKey".to_string(), "ping".to_string()),
            ("identity.sourceIp".to_string(), "127.0.0.1".to_string()),
        ]);
        let stage_variables = BTreeMap::from([("tenant".to_string(), "acme".to_string())]);
        let call = TemplateCall::new(&context, &stage_variables, Some(r#"{"action":"ping"}"#));
        // The rows span what the binding generalises over: a template with no reference at
        // all, each kind of reference a frame has a value for (a member of the context, a
        // nested one, a stage variable), both spellings of a reference, a query of the
        // message, and a `$` that opens none.
        let cases: &[(&str, Option<&str>)] = &[
            (r#"{"statusCode": 200}"#, Some(r#"{"statusCode": 200}"#)),
            ("$input.body", Some(r#"{"action":"ping"}"#)),
            ("${input.body}", Some(r#"{"action":"ping"}"#)),
            ("id=$context.connectionId", Some("id=L0SM9cOFyjUCIhA=")),
            ("$context.routeKey", Some("ping")),
            ("$context.identity.sourceIp", Some("127.0.0.1")),
            ("$stageVariables.tenant", Some("acme")),
            ("$input.json('$.action')", Some("\"ping\"")),
            ("price $9", Some("price $9")),
            // Not rendered: a reference this call has no value for, and a directive this
            // emulator does not perform.
            ("$stageVariables.where", None),
            ("$context.nothing", None),
            ("#foreach($x in $input.path('$.ids'))x#end", None),
        ];
        for (template, expected) in cases {
            let rendered = rendered_template(template, Some(call)).ok();
            assert_eq!(
                rendered.as_deref(),
                *expected,
                "case {template:?}: expected {expected:?}, got {rendered:?}"
            );
        }
    }
}
