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
    // The media type is the method response's own `Content-Type` mapping where it declares
    // one; see [`super::taken_content_type`].
    let mapped_type = taken_content_type(&mut headers);
    let templated = selected
        .response_templates
        .as_ref()
        .is_some_and(|templates| !templates.is_empty());
    let (content_type, body) = if templated || matches!(untemplated, Untemplated::Nothing) {
        // The template is rendered against THIS call, with the payload it reads being the
        // one the INTEGRATION answered rather than the one the client sent.
        let inputs = TemplateInputs {
            payload: answer.payload.clone(),
            ..call.inputs.clone()
        };
        let body =
            response_body(selected, call, &inputs).map_err(|template| UnservedIntegration {
                refusal: API_CONFIGURATION_ERROR,
                reason: format!(
                    "the {} response template of {} bytes is not one this emulator renders: {}",
                    template.content_type, template.bytes, template.reason
                ),
            })?;
        // A templated response is rendered from the api's own records, so the media type it
        // is served with is the mapping's, and `application/json` when none is declared —
        // never the key of the template the caller's `Accept` chose.
        (
            mapped_type.unwrap_or_else(|| JSON_CONTENT_TYPE.to_string()),
            body,
        )
    } else {
        // No template: the answer's own bytes and content type are the method response's,
        // which is what passing a response through means — unless the method response
        // declares a `Content-Type` mapping of its own, which overrides it.
        (
            mapped_type
                .or_else(|| answer.content_type.clone())
                .unwrap_or_else(|| JSON_CONTENT_TYPE.to_string()),
            answer.payload.clone().unwrap_or_default(),
        )
    };
    Ok(IntegratedResponse {
        status,
        content_type,
        body,
        headers,
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
