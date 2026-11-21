// SPDX-License-Identifier: CC0-1.0

//! The GATEWAY RESPONSES of the `execute-api` data plane: every refusal this plane takes,
//! the response type each is answered through, and how one is rendered into the status, body
//! and headers the caller sees.
//!
//! One concern, in one place: the catalog of refusals ([`GatewayRefusal`] and the constants
//! beside it) and the rendering of one ([`refusal_rendering`], which resolves the api's own
//! customization of the response type and otherwise answers API Gateway's own default body).
//! The data plane decides WHICH refusal a call earns; nothing outside this module decides
//! what a refusal looks like on the wire.

use super::*;

/// One refusal of the data plane: the GATEWAY RESPONSE TYPE it is answered through — which
/// decides its status code, and which customization of the API applies to it — and the
/// sentence API Gateway's own default body carries for it.
///
/// The type is the unit of customization (`PutGatewayResponse` takes a `responseType`), so
/// naming it here is what lets a developer's customization reach the caller without this
/// module knowing anything about it.
#[derive(Debug, Clone)]
pub struct GatewayRefusal {
    /// A value of the model's `GatewayResponseType` enum.
    pub(super) response_type: &'static str,
    /// The sentence the uncustomized response body carries, or `None` for a refusal class
    /// the gateway sets NO error message for.
    ///
    /// A gateway response's own default template is
    /// `{"message":$context.error.messageString}`, and `messageString` is "the quoted value
    /// of `$context.error.message`" — so a class with no message of its own renders as the
    /// quoted value of nothing, which is JSON `null`, and its default body is
    /// `{"message":null}` rather than a sentence ([`GatewayRefusal::quoted_message`],
    /// [`default_refusal_body`]).
    ///
    /// Owned-or-borrowed because a few refusals state what the request was missing, which is
    /// read off the request rather than fixed in the binary.
    pub(super) message: Option<Cow<'static, str>>,
    /// The error shape this refusal is the wire form of, reported in
    /// [`ERROR_TYPE_HEADER`], or `None` for a refusal whose exception class nothing in
    /// reach names.
    ///
    /// The class is the one the `execute-api` endpoint itself reports, which is NOT
    /// confined to the exceptions this service's CONTROL-plane model declares: the data
    /// plane is not in that model at all (see the module docs), and it answers a refused
    /// invoke with the exception class of the envelope it sends — the `Forbidden` body
    /// with `ForbiddenException`, the missing-token body with
    /// `MissingAuthenticationTokenException`, the IAM-shaped denial with
    /// `AccessDeniedException`. A client's error handling keys off this header, so
    /// omitting it on a refusal whose class IS known leaves the caller unable to classify
    /// a 403 it can otherwise read only as prose.
    pub(super) error_type: Option<&'static str>,
    /// What a VALIDATION of the request found, as the gateway response's
    /// `$context.error.validationErrorString` reports it; empty for every refusal that is
    /// not the outcome of validating the request against what the method declares.
    pub(super) validation_detail: Cow<'static, str>,
    /// The member API Gateway's own default body carries the sentence under. Every gateway
    /// response's own default template is `{"message":$context.error.messageString}`, so
    /// that is [`MESSAGE_MEMBER`] for all but the AUTHORIZATION refusal, which API Gateway
    /// answers with the IAM-shaped [`AUTHORIZATION_MESSAGE_MEMBER`] body.
    pub(super) message_member: &'static str,
}

impl GatewayRefusal {
    /// The `GatewayResponseType` this refusal is answered through — the one a developer's
    /// `PutGatewayResponse` customization of it would apply to. Read by the caller that
    /// REPORTS a refusal rather than answering it on the wire (`TestInvokeMethod`'s
    /// execution log), so the report names the same gateway response the data plane uses.
    pub fn response_type(&self) -> &'static str {
        self.response_type
    }

    /// The media type the response states for this refusal's body.
    ///
    /// Every gateway response the API renders is `application/json` — that is the content
    /// type its own default template is keyed by ([`JSON_CONTENT_TYPE`]). The one exception
    /// is the refusal the EDGE takes before the api's records are read at all
    /// ([`request_too_large`]), which the endpoint states with the media type a recorded
    /// over-sized invoke was answered with.
    pub(super) fn content_type(&self) -> &'static str {
        if self.response_type == REQUEST_TOO_LARGE_TYPE {
            EDGE_REFUSAL_CONTENT_TYPE
        } else {
            JSON_CONTENT_TYPE
        }
    }

    /// The refusal's own sentence as `$context.error.message` carries it: the text for a
    /// class that sets one, and nothing at all for a class that sets none
    /// ([`GatewayRefusal::message`]).
    pub(super) fn message_text(&self) -> &str {
        self.message.as_deref().unwrap_or_default()
    }

    /// The same message as `$context.error.messageString` carries it — "the quoted value of
    /// `$context.error.message`" — which is a JSON string for a class that sets a message
    /// and JSON `null` for one that sets none ([`GatewayRefusal::message`]).
    pub(super) fn quoted_message(&self) -> String {
        match &self.message {
            Some(message) => ::serde_json::Value::String(message.to_string()),
            None => ::serde_json::Value::Null,
        }
        .to_string()
    }
}

/// The media type the `execute-api` endpoint states for the refusal it takes at the edge;
/// see [`GatewayRefusal::content_type`].
pub(super) const EDGE_REFUSAL_CONTENT_TYPE: &str = "text; charset=utf-8";

/// The API, the stage or the endpoint does not serve this request at all: API Gateway
/// answers `403` with `{"message":"Forbidden"}` for an API id it does not serve, a stage
/// that has no deployment, and an endpoint a developer has turned off.
pub(super) const FORBIDDEN: GatewayRefusal = GatewayRefusal {
    response_type: "ACCESS_DENIED",
    message: Some(Cow::Borrowed("Forbidden")),
    error_type: Some(FORBIDDEN_EXCEPTION),
    validation_detail: Cow::Borrowed(""),
    message_member: MESSAGE_MEMBER,
};

/// Nothing in the DEPLOYMENT matches the path and verb, or the method's authorization
/// needs a credential the request did not carry: both are
/// `{"message":"Missing Authentication Token"}` with `403`, which is what API Gateway
/// answers for a route it cannot place and for an `AWS_IAM` method reached unsigned.
pub(super) const MISSING_AUTHENTICATION_TOKEN: GatewayRefusal = GatewayRefusal {
    response_type: "MISSING_AUTHENTICATION_TOKEN",
    message: Some(Cow::Borrowed("Missing Authentication Token")),
    error_type: Some("MissingAuthenticationTokenException"),
    validation_detail: Cow::Borrowed(""),
    message_member: MESSAGE_MEMBER,
};

/// The method requires an API key and the request presented none this API's records
/// associate with this stage: `403` `{"message":"Forbidden"}` through `INVALID_API_KEY`.
pub(super) const INVALID_API_KEY: GatewayRefusal = GatewayRefusal {
    response_type: "INVALID_API_KEY",
    message: Some(Cow::Borrowed("Forbidden")),
    error_type: Some(FORBIDDEN_EXCEPTION),
    validation_detail: Cow::Borrowed(""),
    message_member: MESSAGE_MEMBER,
};

/// An authorizer's identity source is absent from the request, so the authorizer is not
/// even called, or what it carried is no credential the authorizer could evaluate at all:
/// `401` `{"message":"Unauthorized"}`.
pub(super) const UNAUTHORIZED: GatewayRefusal = GatewayRefusal {
    response_type: "UNAUTHORIZED",
    message: Some(Cow::Borrowed("Unauthorized")),
    error_type: Some(UNAUTHORIZED_EXCEPTION),
    validation_detail: Cow::Borrowed(""),
    message_member: MESSAGE_MEMBER,
};

/// The exception class the endpoint reports a request that authenticated nobody in, which is
/// the class of every refusal answered through the one `401` gateway response type
/// ([`UNAUTHORIZED`], [`EXPIRED_IDENTITY_TOKEN`]).
pub(super) const UNAUTHORIZED_EXCEPTION: &str = "UnauthorizedException";

/// The identity token an authorizer's identity source carried is a signed token whose own
/// `exp` has passed: `401`, carrying the sentence API Gateway words an EXPIRED token with
/// rather than the one a request that presented nothing earns — a holder whose token ran out
/// has to obtain another, which is a different remedy from presenting a credential at all.
///
/// Answered through the same `UNAUTHORIZED` gateway response as [`UNAUTHORIZED`]: that is the
/// one type a request that authenticated nobody is answered through, and a gateway response's
/// own default template renders THIS refusal's sentence
/// (`{"message":$context.error.messageString}`), so one type carries both sentences and a
/// developer's customization of it reaches both. The catalog's `EXPIRED_TOKEN` type is the
/// `403` of an expired SigV4 security token ([`crate::state::default_gateway_status_code`]),
/// which is a credential of the signing protocol rather than a pool-minted identity token.
pub(super) const EXPIRED_IDENTITY_TOKEN: GatewayRefusal = GatewayRefusal {
    response_type: "UNAUTHORIZED",
    message: Some(Cow::Borrowed("The incoming token has expired")),
    error_type: Some(UNAUTHORIZED_EXCEPTION),
    validation_detail: Cow::Borrowed(""),
    message_member: MESSAGE_MEMBER,
};

/// The method's configuration cannot serve the call: no integration on the deployed
/// method, an integration whose type names a backend this emulator cannot call, or a
/// mapping template it cannot evaluate. `500` `{"message":"Internal server error"}`.
pub(super) const API_CONFIGURATION_ERROR: GatewayRefusal = GatewayRefusal {
    response_type: "API_CONFIGURATION_ERROR",
    message: Some(Cow::Borrowed("Internal server error")),
    error_type: Some(INTERNAL_SERVER_ERROR_EXCEPTION),
    validation_detail: Cow::Borrowed(""),
    message_member: MESSAGE_MEMBER,
};

/// The integration's backend had not answered by the integration's own `timeoutInMillis`
/// ("custom timeout between 50 and 29,000 milliseconds", `Integration$timeoutInMillis`), so
/// the call has no backend answer to map: `504` through `INTEGRATION_TIMEOUT`
/// ([`crate::state::default_gateway_status_code`] gives that type's own status), carrying
/// the sentence API Gateway answers an integration that ran out of time with.
///
/// This is what bounds a request to a deployed stage whose integration reaches a backend
/// that runs for its own time ([`lambda_proxy`]): the wait is the api's own stored number
/// rather than anything a request chooses, and a request that reaches it is ANSWERED rather
/// than held open.
pub(super) const INTEGRATION_TIMEOUT: GatewayRefusal = GatewayRefusal {
    response_type: "INTEGRATION_TIMEOUT",
    message: Some(Cow::Borrowed("Endpoint request timed out")),
    error_type: None,
    validation_detail: Cow::Borrowed(""),
    message_member: MESSAGE_MEMBER,
};

/// The method is authorized by an `Authorizer` whose decision this emulator cannot
/// obtain — the authorizer names a function this emulator cannot invoke at all.
/// `500` through `AUTHORIZER_CONFIGURATION_ERROR`, the gateway response type API Gateway
/// answers a call whose authorizer it could not call with.
///
/// The gateway sets NO error message of its own for a refusal the AUTHORIZER produced no
/// decision for, so this response type's default template renders the quoted value of
/// nothing and the body is exactly `{"message":null}` ([`GatewayRefusal::message`]). That is
/// what tells a client an authorizer it must fix from a backend that broke behind a call the
/// gateway DID admit, which carries the `Internal server error` sentence
/// ([`API_CONFIGURATION_ERROR`]) — and the class is that distinction in the header too
/// ([`AUTHORIZER_CONFIGURATION_EXCEPTION`]).
pub(super) const AUTHORIZER_CONFIGURATION_ERROR: GatewayRefusal = GatewayRefusal {
    response_type: "AUTHORIZER_CONFIGURATION_ERROR",
    message: None,
    error_type: Some(AUTHORIZER_CONFIGURATION_EXCEPTION),
    validation_detail: Cow::Borrowed(""),
    message_member: MESSAGE_MEMBER,
};

/// The method's `Authorizer` RAN and produced no decision: its function errored, did not
/// answer in time, or returned something that is not an authorizer output. `500` through
/// `AUTHORIZER_FAILURE`, the gateway response type API Gateway answers an authorizer that
/// failed with — a type of its own, so a developer's customization of it reaches a caller
/// whose authorizer broke without also covering an api that cannot invoke one at all.
///
/// Carries no error message either, for the reason the sibling authorizer refusal records:
/// the uncustomized body of a call no authorizer decided is `{"message":null}`, under that
/// same class ([`AUTHORIZER_CONFIGURATION_ERROR`], [`AUTHORIZER_CONFIGURATION_EXCEPTION`]).
pub(super) const AUTHORIZER_FAILURE: GatewayRefusal = GatewayRefusal {
    response_type: "AUTHORIZER_FAILURE",
    message: None,
    error_type: Some(AUTHORIZER_CONFIGURATION_EXCEPTION),
    validation_detail: Cow::Borrowed(""),
    message_member: MESSAGE_MEMBER,
};

/// The request's content type maps to no integration request template and the
/// integration's `passthroughBehavior` does not let an unmapped body through: `415`
/// `{"message":"Unsupported Media Type"}`.
pub(super) const UNSUPPORTED_MEDIA_TYPE: GatewayRefusal = GatewayRefusal {
    response_type: "UNSUPPORTED_MEDIA_TYPE",
    message: Some(Cow::Borrowed("Unsupported Media Type")),
    error_type: None,
    validation_detail: Cow::Borrowed(""),
    message_member: MESSAGE_MEMBER,
};

/// The documented limit on the payload of one request to a REST API: API Gateway's
/// "Payload size" quota is 10,485,760 bytes, and it is one of the quotas the service lists
/// as not adjustable.
pub const MAX_REQUEST_PAYLOAD_BYTES: usize = 10_485_760;

/// The request's payload is larger than [`MAX_REQUEST_PAYLOAD_BYTES`]: `413` through
/// `REQUEST_TOO_LARGE` ([`crate::state::default_gateway_status_code`] gives that type's own
/// status), carrying the sentence API Gateway answers an over-sized payload with, under the
/// media type the endpoint declares for this refusal ([`EDGE_REFUSAL_CONTENT_TYPE`]) — so the
/// body is that sentence as text rather than the JSON envelope a gateway response of an api
/// renders. The sentence's one variable part is the LIMIT that was enforced (`limit`,
/// [`MAX_REQUEST_PAYLOAD_BYTES`] for a REST API), so a client that reads the bound out of the
/// refusal reads the bound that refused it.
///
/// Refused at the EDGE, with the api's own records unread and the stage, the route, the
/// authorization, the validator and the integration never reached: the limit is on what the
/// endpoint ADMITS, so it cannot depend on what the api or the method declares — and a
/// developer's customization of a gateway response, which is a record OF an api, therefore
/// does not reach it.
pub(super) fn request_too_large(limit: usize) -> GatewayRefusal {
    GatewayRefusal {
        response_type: REQUEST_TOO_LARGE_TYPE,
        message: Some(Cow::Owned(format!(
            "HTTP content length exceeded {limit} bytes."
        ))),
        error_type: None,
        validation_detail: Cow::Borrowed(""),
        message_member: MESSAGE_MEMBER,
    }
}

/// The refusal a request whose payload is over [`MAX_REQUEST_PAYLOAD_BYTES`] earns, or
/// `None` for one within it.
///
/// Read at the funnel every invoke enters ([`serve`]), so it is the ENDPOINT's limit: it
/// bounds what arrives from a client, not what one integration passes on to another.
///
/// A body the transport did not buffer in memory is one whose size is not known here, and
/// is admitted: the limit refuses a payload that IS over it rather than one that might be.
pub(super) fn oversized_payload(req: &RtRequest) -> Option<GatewayRefusal> {
    let sent = req.body().bytes()?.len();
    (sent > MAX_REQUEST_PAYLOAD_BYTES).then(|| request_too_large(MAX_REQUEST_PAYLOAD_BYTES))
}

/// The gateway response type a payload the method's `Model` does not admit is answered
/// through; see [`bad_request_body`].
pub(super) const BAD_REQUEST_BODY_TYPE: &str = "BAD_REQUEST_BODY";

/// The gateway response type a request missing a parameter the method declares required is
/// answered through; see [`bad_request_parameters`].
pub(super) const BAD_REQUEST_PARAMETERS_TYPE: &str = "BAD_REQUEST_PARAMETERS";

/// The two gateway response types the method's REQUEST VALIDATOR produces.
///
/// Their default bodies are the VALIDATOR's own serialization, which writes a SPACE after
/// the member separator — `{"message": "Invalid request body"}`, as the Developer Guide's
/// request-validation walkthrough shows both of them — where every OTHER default body is
/// what a gateway response's own default template produces
/// ([`crate::state::DEFAULT_GATEWAY_RESPONSE_TEMPLATE`],
/// `{"message":$context.error.messageString}`), which is compact. One list, read by
/// [`default_refusal_body`], so the two renderings cannot drift apart.
pub(super) const VALIDATION_RESPONSE_TYPES: [&str; 2] = [BAD_REQUEST_BODY_TYPE, BAD_REQUEST_PARAMETERS_TYPE];

/// The gateway response type an over-sized payload is refused through; see
/// [`request_too_large`].
pub(super) const REQUEST_TOO_LARGE_TYPE: &str = "REQUEST_TOO_LARGE";

/// The method's request validator declares `validateRequestBody` and the request's payload
/// does not satisfy the JSON Schema of the `Model` the method maps its content type to:
/// `400` `{"message": "Invalid request body"}` through `BAD_REQUEST_BODY`.
///
/// The sentence is FIXED — it names neither the model nor what about the payload failed.
/// WHAT failed is the validation's own report, which a gateway response template reads
/// through `$context.error.validationErrorString`: `detail` is that report, and it is the
/// validator's own finding about THIS payload rather than a sentence written here.
pub(super) fn bad_request_body(detail: String) -> GatewayRefusal {
    GatewayRefusal {
        response_type: BAD_REQUEST_BODY_TYPE,
        message: Some(Cow::Borrowed("Invalid request body")),
        error_type: Some("BadRequestException"),
        validation_detail: Cow::Owned(detail),
        message_member: MESSAGE_MEMBER,
    }
}

/// The usage plan metering this request's API key admits no further request at this
/// instant: `429` `{"message":"Too Many Requests"}` through `THROTTLED`. The plan's
/// numeric limits are what decide the refusal, not what it says.
pub(super) const THROTTLED: GatewayRefusal = GatewayRefusal {
    response_type: "THROTTLED",
    message: Some(Cow::Borrowed("Too Many Requests")),
    error_type: Some("TooManyRequestsException"),
    validation_detail: Cow::Borrowed(""),
    message_member: MESSAGE_MEMBER,
};

/// The usage plan metering this request's API key admits no further request in the current
/// metering period: `429` `{"message":"Limit Exceeded"}` through `QUOTA_EXCEEDED`. The
/// plan's quota is what decides the refusal, not what it says.
pub(super) const QUOTA_EXCEEDED: GatewayRefusal = GatewayRefusal {
    response_type: "QUOTA_EXCEEDED",
    message: Some(Cow::Borrowed("Limit Exceeded")),
    error_type: Some("LimitExceededException"),
    validation_detail: Cow::Borrowed(""),
    message_member: MESSAGE_MEMBER,
};

/// The method's request validator declares `validateRequestParameters` and the request
/// does not carry every request parameter the method declares REQUIRED: `400` through
/// `BAD_REQUEST_PARAMETERS`.
///
/// The sentence's fixed part states the condition and its variable part is the list of
/// parameter NAMES the request was missing, read off the method's own
/// `requestParameters` declarations — which is what tells the caller what to add, and
/// which is also the validation's whole finding, so it is reported as the detail too.
pub(super) fn bad_request_parameters(missing: &[String]) -> GatewayRefusal {
    let named = missing.join(", ");
    GatewayRefusal {
        response_type: BAD_REQUEST_PARAMETERS_TYPE,
        message: Some(Cow::Owned(format!(
            "Missing required request parameters: [{named}]"
        ))),
        error_type: Some("BadRequestException"),
        validation_detail: Cow::Owned(format!("[{named}]")),
        message_member: MESSAGE_MEMBER,
    }
}

/// The api's own RESOURCE POLICY does not let this request invoke the method: `403`,
/// carrying the sentence AWS refuses an unauthorized invoke with
/// ([`resource_policy::denial`]).
///
/// Answered through `ACCESS_DENIED`, the gateway response type an authorization failure is
/// customized under, so a developer's `PutGatewayResponse` for it reaches this refusal too.
/// The body's member is the IAM-shaped [`AUTHORIZATION_MESSAGE_MEMBER`] rather than the
/// [`MESSAGE_MEMBER`] every other gateway response carries, because that is the body API
/// Gateway answers an authorization refusal with.
pub(super) fn access_denied(message: String) -> GatewayRefusal {
    GatewayRefusal {
        response_type: "ACCESS_DENIED",
        message: Some(Cow::Owned(message)),
        error_type: Some("AccessDeniedException"),
        validation_detail: Cow::Borrowed(""),
        message_member: AUTHORIZATION_MESSAGE_MEMBER,
    }
}

/// The SigV4 credential an `AWS_IAM` method's caller presented does not authenticate: the
/// access key id names no credential this emulator holds, or the signature is not the one
/// that credential's secret produces over this request
/// ([`sigv4_authentication::Unauthenticated`]).
///
/// Answered through `INVALID_SIGNATURE` — the gateway response type a request whose
/// signature does not conform is customized under, whose own documented status is `403`
/// ([`crate::state::default_gateway_status_code`]) — carrying the sentence AWS words that
/// class with and the exception class the endpoint reports it in, so a client tells a
/// credential it must replace from a signature it must recompute.
pub(super) fn invalid_signature(class: sigv4_authentication::Unauthenticated) -> GatewayRefusal {
    let error_type = class.error_type();
    GatewayRefusal {
        response_type: "INVALID_SIGNATURE",
        error_type: Some(error_type),
        message: Some(class.message()),
        validation_detail: Cow::Borrowed(""),
        message_member: MESSAGE_MEMBER,
    }
}

/// The request was refused by the WAFv2 web ACL in front of the stage: `403` through
/// `WAF_FILTERED`, the gateway response type API Gateway answers a request its web ACL
/// blocked with ([`crate::state::default_gateway_status_code`] gives that type's own
/// status).
///
/// Answered through a gateway response, so a developer's `PutGatewayResponse`
/// customization of `WAF_FILTERED` reaches a blocked caller — and so a `BlockAction`
/// that configures its OWN `CustomResponse` bypasses this entirely, because then WAF
/// serves the response itself ([`EdgeOutcome`]).
pub(super) const WAF_FILTERED: GatewayRefusal = GatewayRefusal {
    response_type: "WAF_FILTERED",
    message: Some(Cow::Borrowed("Forbidden")),
    error_type: Some(FORBIDDEN_EXCEPTION),
    validation_detail: Cow::Borrowed(""),
    message_member: MESSAGE_MEMBER,
};

/// The header API Gateway names the error shape of a refused request in, which is what an
/// SDK reads a restJson1 error code out of.
///
/// Spelled as API Gateway spells it: an HTTP field name compares case-insensitively, so the
/// casing is immaterial to a client reading the header off the wire, but a `TestInvokeMethod`
/// report carries the headers as MEMBERS of a JSON map, where the name is compared verbatim.
pub(super) const ERROR_TYPE_HEADER: &str = "x-amzn-ErrorType";

/// The exception class every refusal whose body is the `Forbidden` envelope is the wire
/// form of: the endpoint refusing the call outright, whether because the API, the stage or
/// the endpoint does not serve it ([`FORBIDDEN`]), because the method requires an API key
/// the request did not present ([`INVALID_API_KEY`]), or because the web ACL in front of
/// the stage blocked it ([`WAF_FILTERED`]). One sentence, one status, one class: a caller
/// that reads the class reads the same one for all three, as the recorded keyless invoke of
/// a key-required method shows.
pub(super) const FORBIDDEN_EXCEPTION: &str = "ForbiddenException";

/// The exception class the endpoint reports every refusal of a call its AUTHORIZER produced
/// no decision for: an authorizer that could not be invoked at all
/// ([`AUTHORIZER_CONFIGURATION_ERROR`]) and one whose run produced none
/// ([`AUTHORIZER_FAILURE`]). The authorizer is the part of the configuration a caller's
/// operator has to fix, and the endpoint names it rather than the generic server fault, as
/// the recorded invoke of a stage whose authorizer function failed shows.
pub(super) const AUTHORIZER_CONFIGURATION_EXCEPTION: &str = "AuthorizerConfigurationException";

/// The exception class the endpoint reports a refusal whose cause is the method's own
/// configuration rather than its authorizer: an integration the stage cannot perform
/// ([`API_CONFIGURATION_ERROR`]). A client's retry and alarm handling keys off this header,
/// so a 5xx left without it is an untyped failure a caller can read only as prose.
pub(super) const INTERNAL_SERVER_ERROR_EXCEPTION: &str = "InternalServerErrorException";

/// The `$context` variables a GATEWAY RESPONSE's template names, and what each one stands
/// for in the refusal being answered.
///
/// A gateway response is answered for a request that reached no integration, so the
/// variables it can resolve are the ones describing the REFUSAL. API Gateway's own default
/// template for every type is `{"message":$context.error.messageString}` — the
/// `messageString` form is "the quoted value of `$context.error.message`", quotes
/// included, which is why that template produces a JSON object — beside the message
/// itself, the gateway response type the refusal is answered through, and the validation
/// detail, which is a PLAIN string and which only a refusal that VALIDATED the request
/// has ([`GatewayRefusal::validation_detail`]).
pub(super) fn gateway_context(refusal: &GatewayRefusal) -> Vec<(&'static str, String)> {
    // Longest path first: `context.error.message` is a PREFIX of
    // `context.error.messageString`, so substituting it first would leave `String` behind.
    vec![
        ("context.error.validationErrorString", refusal.validation_detail.to_string()),
        ("context.error.messageString", refusal.quoted_message()),
        ("context.error.responseType", refusal.response_type.to_string()),
        ("context.error.message", refusal.message_text().to_string()),
    ]
}

/// Emulator resource guard, NOT an AWS quota: the largest body one gateway-response template
/// renders to.
///
/// The same bound the sibling renderer of this language works under
/// (`super::mapping_template`'s own rendered-body cap, a small multiple of a stored
/// template's character cap): a gateway template is rendered while SERVING a request, and the
/// values it substitutes — the refusal's message and its validation detail — carry text a
/// caller influenced, so the body is capped as well as built in one pass. Generous enough
/// that no body a gateway response would answer is refused for its size; a template that
/// renders past it answers API Gateway's own default body for the response type
/// ([`default_refusal_body`]), which is what a template this emulator cannot render answers.
pub(super) const MAX_RENDERED_GATEWAY_BYTES: usize = 8 * crate::ops::put_integration::MAX_TEMPLATE_CHARS;

/// The body a gateway response's `application/json` template renders to for `refusal`, or
/// `None` when this emulator cannot render it.
///
/// The template's `$context` references are resolved from [`gateway_context`], in both
/// spellings the language writes a reference in (`$context.error.message` and
/// `${context.error.message}`); a reference to anything else is left in the body exactly as
/// the template wrote it, which is what the language itself does with a reference it cannot
/// resolve. A template carrying a DIRECTIVE is a template that has to be EXECUTED, which
/// this emulator does not do: it renders nothing rather than a body it guessed at, and the
/// caller gets API Gateway's own default body for the response type.
///
/// ONE left-to-right pass over the TEMPLATE, so a value that has been substituted is never
/// scanned again, and the body is bounded by [`MAX_RENDERED_GATEWAY_BYTES`]. The values come
/// from the refusal, whose message and validation detail carry text the request influenced,
/// so rewriting the whole template once per name would let one value carry another name's
/// reference and make the body the PRODUCT of the values' sizes rather than their sum. It is
/// the same no-rescan contract [`expanded_uri`] and [`resolved_references`] state, for the
/// same reason.
pub(super) fn rendered_gateway_template(template: &str, refusal: &GatewayRefusal) -> Option<String> {
    if carries_velocity_directive(template) {
        return None;
    }
    // Both spellings of each name, built once rather than once per reference; longest name
    // first, as [`gateway_context`] orders them, so `$context.error.messageString` is
    // recognized before the `$context.error.message` it opens with.
    let references: Vec<(String, String, String)> = gateway_context(refusal)
        .into_iter()
        .map(|(path, value)| (format!("{{{path}}}"), path.to_string(), value))
        .collect();
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(dollar) = rest.find('$') {
        let (before, from_dollar) = rest.split_at(dollar);
        rendered.push_str(before);
        // `$` is ASCII, so what follows it starts at a character boundary.
        let after_dollar = from_dollar.get(1..).unwrap_or_default();
        let substituted = references.iter().find_map(|(braced, plain, value)| {
            let after = after_dollar
                .strip_prefix(braced.as_str())
                .or_else(|| after_dollar.strip_prefix(plain.as_str()))?;
            Some((value, after))
        });
        match substituted {
            Some((value, after)) => {
                rendered.push_str(value);
                rest = after;
            }
            // A reference this refusal resolves nothing for travels on as the template
            // wrote it: scanning continues after the `$`, so the rest of it is copied as
            // text rather than examined as a reference again.
            None => {
                rendered.push('$');
                rest = after_dollar;
            }
        }
        // The length of what has been built, not a walk over it, checked after each value:
        // one value may carry the total past the cap.
        if rendered.len() > MAX_RENDERED_GATEWAY_BYTES {
            return None;
        }
    }
    rendered.push_str(rest);
    Some(rendered)
}

/// What one gateway-response refusal renders to, as the API answers it.
#[derive(Debug)]
pub struct RenderedRefusal {
    /// The status code the response is sent with.
    pub status: u16,
    /// The body it carries.
    pub body: Vec<u8>,
    /// The headers the answer carries: the exception class the refusal is the wire form of
    /// ([`ERROR_TYPE_HEADER`]) and whatever the API's customization of this gateway response
    /// maps onto the response, by the name the mapping declared; see [`refusal_rendering`].
    pub headers: BTreeMap<String, String>,
}

/// The status, body and headers one gateway-response refusal renders to, as the API answers
/// it.
///
/// The status code is the API's customization of this response type when it has one, then
/// its customization of the whole class (`DEFAULT_4XX` / `DEFAULT_5XX`), then the status
/// code the response type itself documents. The body is the customized
/// `application/json` template with its `$context` references resolved
/// ([`rendered_gateway_template`]), and otherwise the default `{"message": …}` body
/// ([`default_refusal_body`]) — which is what a template this emulator cannot render falls
/// back to.
///
/// The HEADERS are that same customization's `responseParameters`: a key is a mapping
/// expression naming what the customization writes on the response, and a gateway response
/// writes HEADERS ([`crate::ops::put_gateway_response::GATEWAY_RESPONSE_HEADER_PREFIXES`]),
/// so a mapping declared there reaches the caller on the response the type is served with.
/// The mapping this resolves is the documented static value in single quotes
/// ([`static_mapping_literal`]) — the literal between the quotes is the header's value;
/// a mapping that reads a value off the REQUEST names nothing here, because a gateway
/// response is rendered from the refusal rather than from the request that earned it.
///
/// Separate from [`refusal_response`] because the data plane SENDS this rendering while
/// `TestInvokeMethod` REPORTS it as the simulated response, and a developer's
/// customization must reach both.
pub fn refusal_rendering(api: Option<&RestApiEntry>, refusal: GatewayRefusal) -> RenderedRefusal {
    let documented = crate::state::default_gateway_status_code(refusal.response_type)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(UNPLACEABLE_REFUSAL_STATUS);
    let class_default = if documented < 500 { DEFAULT_4XX } else { DEFAULT_5XX };
    let customized = api.and_then(|api| {
        api.gateway_responses
            .get(refusal.response_type)
            .or_else(|| api.gateway_responses.get(class_default))
    });
    let status = customized
        .and_then(|response| response.status_code.as_deref())
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(documented);
    let templated = customized
        .and_then(|response| response.response_templates.as_ref())
        .and_then(|templates| templates.get(JSON_CONTENT_TYPE))
        .and_then(|template| rendered_gateway_template(template, &refusal));
    let body = match templated {
        Some(template) => template.into_bytes(),
        None => default_refusal_body(&refusal),
    };
    let mut headers = gateway_response_headers(customized);
    // The response NAMES the error shape it is the wire form of, in the header an SDK reads a
    // restJson1 error code out of ([`ERROR_TYPE_HEADER`]), for every refusal whose exception
    // class is known ([`GatewayRefusal::error_type`]). Part of the RENDERING rather than of
    // the wire response, because it is a header of the answer the refusal produces: the
    // simulated response a `TestInvokeMethod` reports carries it exactly as the served one
    // does, which is what lets a client classify a refusal it tested for.
    if let Some(error_type) = refusal.error_type {
        headers.insert(ERROR_TYPE_HEADER.to_string(), error_type.to_string());
    }
    RenderedRefusal {
        status,
        body,
        headers,
    }
}

/// The headers the gateway-response customization `customized` writes on the response it is
/// served with; see [`refusal_rendering`] for which mappings resolve.
pub(super) fn gateway_response_headers(
    customized: Option<&crate::ops::types::GetGatewayResponseOutput>,
) -> BTreeMap<String, String> {
    let mut mapped = BTreeMap::new();
    for (key, value) in customized
        .and_then(|response| response.response_parameters.as_ref())
        .into_iter()
        .flatten()
    {
        let Some(name) = crate::ops::put_gateway_response::GATEWAY_RESPONSE_HEADER_PREFIXES
            .iter()
            .find_map(|prefix| key.strip_prefix(prefix))
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let literal = static_mapping_literal(value);
        if let Some(literal) = literal {
            mapped.insert(name.to_string(), literal.to_string());
        }
    }
    mapped
}

/// API Gateway's OWN default body for `refusal`: the refusal's message under the member the
/// response type carries it in, as that type's own default template
/// ([`crate::state::DEFAULT_GATEWAY_RESPONSE_TEMPLATE`]) renders it — so a class the gateway
/// sets no message for carries the JSON `null` the template's quoted-value reference
/// produces rather than a sentence ([`GatewayRefusal::message`]).
///
/// Serialized by hand rather than through a JSON writer because the SEPARATOR is part of
/// what API Gateway sends: the two bodies its request validator serializes itself carry a
/// space after it ([`VALIDATION_RESPONSE_TYPES`], written by [`serialized_envelope`]) and
/// the ones its gateway-response template renders carry none, so a client comparing or
/// hashing the payload sees the bytes the service sends.
/// The member name and the sentence are both written as JSON strings, so a sentence
/// carrying a quote or a backslash is escaped as the format requires.
pub(super) fn default_refusal_body(refusal: &GatewayRefusal) -> Vec<u8> {
    // A refusal the endpoint states with a TEXT media type ([`EDGE_REFUSAL_CONTENT_TYPE`])
    // carries the SENTENCE itself: the body a response declares a media type for has to be
    // readable as that type, so a JSON envelope under `text` would be a body no client could
    // parse by what the response says it is.
    if refusal.content_type() != JSON_CONTENT_TYPE {
        return refusal.message_text().as_bytes().to_vec();
    }
    if VALIDATION_RESPONSE_TYPES.contains(&refusal.response_type) {
        return serialized_envelope(refusal.message_member, refusal.message_text());
    }
    let member = ::serde_json::Value::String(refusal.message_member.to_string()).to_string();
    // The message as the response type's own default template renders it, so a class the
    // gateway sets none for answers the JSON `null` that template produces.
    let message = refusal.quoted_message();
    format!("{{{member}:{message}}}").into_bytes()
}

/// One sentence under one member, serialized the way API Gateway serializes a body it
/// renders ITSELF rather than through a gateway response's own default template: a SPACE
/// after the member separator.
///
/// The two renderings are what the service sends for two different classes of answer — the
/// template's is compact ([`crate::state::DEFAULT_GATEWAY_RESPONSE_TEMPLATE`],
/// `{"message":$context.error.messageString}`) — so both live here rather than in each
/// caller, and a client comparing or hashing the payload sees the bytes the service sends.
/// The member name and the sentence are written as JSON strings, so one carrying a quote or
/// a backslash is escaped as the format requires.
pub(super) fn serialized_envelope(member: &str, message: &str) -> Vec<u8> {
    let member = ::serde_json::Value::String(member.to_string()).to_string();
    let message = ::serde_json::Value::String(message.to_string()).to_string();
    format!("{{{member}: {message}}}").into_bytes()
}
