// SPDX-License-Identifier: CC0-1.0

//! The INTEGRATION RESPONSE and how the integration's own responses MAP it to the method
//! response: the step between performing an integration and serving what it answered.
//!
//! A step of its own because it runs ONCE PER REQUEST and not once per backend answer. What
//! a stage's cache cluster holds is the integration response ([`IntegrationAnswer`]) — the
//! status, media type, headers and payload the integration answered — so a request the
//! cluster answers is mapped here exactly as the request that filled the entry was, with
//! ITS own `$context` and parameters. Anything the mapping derives from the request being
//! served therefore names that request, on a cache hit as on a miss.
//!
//! Three kinds of answer reach this, and [`AnswerMapping`] says which an entry holds: a
//! BACKEND answer, whose `selectionPattern` picks the integration response that maps it; a
//! `MOCK`'s own rendered integration request, whose `statusCode` names that response; and
//! an answer nothing maps — a proxy integration's response, and the response of an endpoint
//! a request was passed on to — which IS the method response.
//!
//! What the integration response that maps an answer may say about the method response is the
//! status it is keyed by, the headers its `responseParameters` map and the body its
//! `responseTemplates` render — and, stated by that template as it renders, an OVERRIDE of a
//! header or of the status ([`overridden_status`]) and the FORM the body is served in, which
//! its `contentHandling` converts ([`content_handling`]).

use super::*;

/// The method response `answer` maps to under the integration the method declares.
///
/// `method` is the method as the STAGE serves it now: the mapping is the api's own records,
/// so a held answer is mapped by the records in force when it is served rather than by a
/// copy taken when it was stored.
pub fn mapped(
    method: Option<&MethodEntry>,
    call: &MethodRequest,
    answer: &IntegrationAnswer,
) -> Result<IntegratedResponse, UnservedIntegration> {
    match &answer.mapping {
        // Nothing maps it: "the integration response is passed through", so the status,
        // media type, headers and body the answer carries are the method response's.
        AnswerMapping::Passed(status) => Ok(IntegratedResponse {
            status: *status,
            content_type: answer
                .content_type
                .clone()
                .unwrap_or_else(|| JSON_CONTENT_TYPE.to_string()),
            body: answer.payload.clone().unwrap_or_default(),
            headers: answer.headers.clone(),
        }),
        AnswerMapping::Backend(status) => {
            let integration = mapping_integration(method)?;
            let Some(selected) = aws_integration::selected_response(integration, *status) else {
                return Err(UnservedIntegration {
                    refusal: API_CONFIGURATION_ERROR,
                    reason: format!(
                        "the integration has no integration response for the {status} the \
                         backend answered, so the call maps to no method response"
                    ),
                });
            };
            rendered(method, selected, call, answer, Untemplated::TheAnswer)
        }
        AnswerMapping::Mock(code) => {
            let integration = mapping_integration(method)?;
            // The status the rendered integration request named is the integration response
            // the call maps to, and a rendering that named none maps to the DEFAULT
            // integration response — the one with no `selectionPattern`, since a `MOCK`
            // produces no backend response for a pattern to match.
            let selected = code
                .as_ref()
                .and_then(|code| {
                    integration
                        .integration_responses
                        .as_ref()
                        .and_then(|responses| responses.get(code))
                })
                .or_else(|| default_integration_response(integration));
            let Some(selected) = selected else {
                return Err(UnservedIntegration {
                    refusal: API_CONFIGURATION_ERROR,
                    reason: "the MOCK integration has no integration response for this call, \
                             so it maps to no method response"
                        .to_string(),
                });
            };
            rendered(method, selected, call, answer, Untemplated::Nothing)
        }
    }
}

/// The integration whose own responses map the answer, or the configuration error a method
/// carrying none is answered through.
fn mapping_integration(
    method: Option<&MethodEntry>,
) -> Result<&GetIntegrationOutput, UnservedIntegration> {
    method
        .and_then(|method| method.integration.as_ref())
        .ok_or_else(|| UnservedIntegration {
            refusal: API_CONFIGURATION_ERROR,
            reason: "the method carries no integration, so there is nothing to call".to_string(),
        })
}

/// What the method response carries when the integration response that maps the answer
/// declares no response template at all.
#[derive(Debug)]
enum Untemplated {
    /// Nothing: a `MOCK` answers no body where no template renders one.
    Nothing,
    /// The answer's own bytes and media type, which is what passing a backend response
    /// through unmapped means.
    TheAnswer,
}

/// The method response the integration response `selected` maps `answer` to: the status
/// `selected` names, the headers it maps, and the body its template renders against THIS
/// call.
fn rendered(
    method: Option<&MethodEntry>,
    selected: &GetIntegrationResponseOutput,
    call: &MethodRequest,
    answer: &IntegrationAnswer,
    untemplated: Untemplated,
) -> Result<IntegratedResponse, UnservedIntegration> {
    let Some(status) = selected
        .status_code
        .as_deref()
        .and_then(|code| code.parse::<u16>().ok())
    else {
        return Err(UnservedIntegration {
            refusal: API_CONFIGURATION_ERROR,
            reason: "the integration response this call maps to carries no status code the \
                     response can be sent with"
                .to_string(),
        });
    };
    let mut headers = mapped_response_headers(method, selected, status, &call.inputs, answer);
    let templated = selected
        .response_templates
        .as_ref()
        .is_some_and(|templates| !templates.is_empty());
    // The response template's own rendering, and what it STATED about this response: a
    // response template may name a header and the status as well as the body
    // ([`mapping_template::Overrides`]). Rendered before the headers are read off the
    // mappings, so an override of one of them has the last word.
    let mut stated = mapping_template::Overrides::default();
    let rendered_body = if templated || matches!(untemplated, Untemplated::Nothing) {
        // The template is rendered against THIS call, with the payload it reads being the
        // one the INTEGRATION answered rather than the one the client sent.
        let inputs = TemplateInputs {
            payload: answer.payload.clone(),
            ..call.inputs.clone()
        };
        let (body, overrides) =
            response_body(selected, call, &inputs).map_err(|template| UnservedIntegration {
                refusal: API_CONFIGURATION_ERROR,
                reason: format!(
                    "the {} response template of {} bytes is not one this emulator renders: {}",
                    template.content_type, template.bytes, template.reason
                ),
            })?;
        stated = overrides;
        Some(body)
    } else {
        None
    };
    // An override is final: a header the response template stated REPLACES what the
    // integration response's own `responseParameters` mapped onto that name, and a name
    // nothing mapped is carried by the override on its own — which is what overriding a
    // response parameter means.
    for (name, value) in &stated.header {
        headers.insert(name.clone(), vec![value.clone()]);
    }
    // The media type is the method response's own `Content-Type` mapping where it declares
    // one; see [`super::taken_content_type`].
    let mapped_type = taken_content_type(&mut headers);
    let (content_type, body) = match rendered_body {
        // A templated response is rendered from the api's own records, so the media type it
        // is served with is the mapping's, and `application/json` when none is declared —
        // never the key of the template the caller's `Accept` chose.
        Some(body) => (
            mapped_type.unwrap_or_else(|| JSON_CONTENT_TYPE.to_string()),
            body,
        ),
        // No template: the answer's own bytes and content type are the method response's,
        // which is what passing a response through means — unless the method response
        // declares a `Content-Type` mapping of its own, which overrides it.
        None => (
            mapped_type
                .or_else(|| answer.content_type.clone())
                .unwrap_or_else(|| JSON_CONTENT_TYPE.to_string()),
            answer.payload.clone().unwrap_or_default(),
        ),
    };
    // The FORM the body is served in: the form this request's `Accept` names, which the
    // integration response's own `contentHandling` overrides ([`content_handling`]). The
    // payload's own media type is the one it is SERVED under — the template's rendering is
    // served under the mapped type and not under whatever the backend answered — and which of
    // those media types is binary the api's `binaryMediaTypes` say.
    let body = content_handling::converted(
        body,
        selected.content_handling.as_deref(),
        &content_type,
        call.accepted.as_deref(),
        &call.proxy.binary_media_types,
        content_handling::THE_METHOD_RESPONSE,
    )?;
    Ok(IntegratedResponse {
        status: overridden_status(&stated, status)?,
        content_type,
        body,
        headers,
    })
}

/// The status the response is sent with: the one the response template STATED where it stated
/// one, and the integration response's own `statusCode` otherwise.
///
/// An override of the status is what lets one integration response answer a call with a status
/// of its own — a backend's `200` served as a `400`, say — so it has the last word over the
/// code the selected integration response is keyed by. A value that is no HTTP status is a
/// configuration error of the template rather than a response sent with a status nothing
/// names.
fn overridden_status(
    stated: &mapping_template::Overrides,
    status: u16,
) -> Result<u16, UnservedIntegration> {
    let Some(overriding) = stated.status.as_deref() else {
        return Ok(status);
    };
    overriding
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|code| (100..=599).contains(code))
        .ok_or_else(|| UnservedIntegration {
            refusal: API_CONFIGURATION_ERROR,
            reason: format!(
                "the response template overrides the status with {overriding:?}, which is no \
                 status a response can be sent with"
            ),
        })
}

/// A PROXY integration's own response as an integration response nothing maps: the function
/// returned the method response itself, so there is no mapping for a later request to apply
/// again.
pub(super) fn passed_answer(served: IntegratedResponse) -> IntegrationAnswer {
    IntegrationAnswer {
        mapping: AnswerMapping::Passed(served.status),
        content_type: Some(served.content_type),
        payload: Some(served.body),
        headers: served.headers,
    }
}

/// The response another endpoint answered a request this plane PASSED ON, as an integration
/// response nothing maps — or `None` for a response whose body is not readable as bytes,
/// which is nothing a later request could be served.
///
/// `content-length` is left off: it is computed from the body when the answer is served
/// ([`super::response_with`]), so holding the one this response carried would be a second
/// copy of a derived value.
pub(super) fn served_answer(served: &RtResponse) -> Option<IntegrationAnswer> {
    let payload = served.body().bytes()?.to_vec();
    let mut headers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut content_type = None;
    for (name, value) in served.headers().iter() {
        if name.eq_ignore_ascii_case(CONTENT_TYPE_HEADER) {
            content_type = Some(value.to_string());
            continue;
        }
        if name.eq_ignore_ascii_case("content-length") {
            continue;
        }
        headers
            .entry(name.to_string())
            .or_default()
            .push(value.to_string());
    }
    Some(IntegrationAnswer {
        mapping: AnswerMapping::Passed(served.status().as_u16()),
        content_type,
        payload: Some(payload),
        headers,
    })
}

/// The response a BACKEND answered an integration request, as an integration response the
/// integration's OWN RESPONSES map — or `None` for a response whose body is not readable as
/// bytes, which is no answer to map.
///
/// The one thing it differs from [`served_answer`] in is the mapping: the status the backend
/// answered is what `IntegrationResponse$selectionPattern` is matched against ("for all other
/// HTTP and Amazon Web Services back ends, the HTTP status code is matched"), where a
/// passed-on response is the method response already. Everything else about an answer is read
/// the one way, so a backend answer and a passed-on one cannot be read two ways; the headers
/// travel because a `responseParameters` mapping names one of them as its source, not because
/// any of them is sent to the caller.
pub(super) fn backend_answer(answered: &RtResponse) -> Option<IntegrationAnswer> {
    Some(IntegrationAnswer {
        mapping: AnswerMapping::Backend(answered.status().as_u16()),
        ..served_answer(answered)?
    })
}
