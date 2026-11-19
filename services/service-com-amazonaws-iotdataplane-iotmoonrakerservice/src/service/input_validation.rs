// SPDX-License-Identifier: CC0-1.0

//! How this data plane answers a request the FRAMEWORK refused before any handler ran — a
//! value outside a `@length`, `@range` or `@pattern` the model declares, or a `@required`
//! member that was absent.
//!
//! # The rules
//!
//! * The NAMES this plane addresses a resource by are its own identity rules, and it states
//!   each of them in its own fixed sentence rather than in a diagnostic about a constraint
//!   walk:
//!   - a `clientId` outside what the model admits is refused `InvalidRequestException`
//!     (HTTP 400) with the one client-id sentence ([`crate::state::CLIENT_ID_NOT_VALID`]),
//!     which states both rules a client id has to satisfy whichever of them the value broke;
//!   - a `thingName` outside what the model admits names no thing this plane can resolve, and
//!     a shadow request resolves the thing before it looks at the shadow — so an operation the
//!     model gives a not-found error ([`THING_NOT_FOUND_OPERATIONS`]) reports it as the ABSENT
//!     resource the request addressed, `ResourceNotFoundException`, with the one thing-name
//!     sentence ([`crate::state::INVALID_THING_NAME`]). `UpdateThingShadow`, the fourth
//!     operation addressing a thing, declares no not-found error at all, so for it such a name
//!     stays the front door's bad request.
//! * Every other front-door refusal is this plane's one terse sentence
//!   ([`crate::state::BAD_REQUEST`]) under the client-input error its model binds to every
//!   operation, carrying nothing of the request — not the member, not the bound it crossed and
//!   not the value the caller sent.
//! * EVERY refusal body this plane writes carries the `traceId` member beside `message`, the
//!   per-request correlation id it mints for every envelope ([`crate::state::new_trace_id`]) —
//!   a body a handler serialized already does ([`crate::ops::types`]), and one the framework
//!   rendered is completed here, so a client that reads `traceId` out of an error finds it
//!   whichever layer answered.
//! * A refusal named in one of this service's OWN error shapes is answered with the status the
//!   model binds to that shape ([`MODELLED_ERRORS`]). The framework's pre-handler renderer
//!   states one status for every constraint refusal, which is right for the shape it would
//!   have named and wrong for a shape whose modelled status is another.
//!
//! # Why two seams
//!
//! The centralized walk is deserialize -> validate -> handle
//! (`vendor/smithy-server-runtime/src/operation.rs`), so a handler runs only AFTER modelled
//! constraints are enforced and cannot word their refusal: the words are asked for at
//! `Service::override_refusal`, which is handed the violations alone — no request, no
//! operation — and can choose only an error code and a sentence.
//!
//! So the words are chosen at that seam ([`stated_refusal`]), reading back the operation the
//! request was routed to from the scope the second seam opens, and the BODY and the STATUS are
//! completed at the one seam that sees a whole rendered response: the operation wrapper
//! `Service::override_operation` hands back ([`REFUSAL_ENVELOPE`]), which serves the operation
//! the routing table names and so leaves the whole walk, every handler's answer and every
//! modelled status to the framework. Neither the plumbing nor the body rewriting is this
//! service's own: they are `service_common::restated_refusal` and
//! `service_common::error_detail_member`.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use ::aws_smithy_schema_server::constraints::Violation;
use ::aws_smithy_types::body::SdkBody;
use ::service_common::restated_refusal::{served_operation, RestatedRefusalOperation};
use ::smithy_server_runtime::{Refusal, Refused};

/// The error code this service refuses a bad request with, which is the error its model binds
/// to every operation for one (`smithy_query(query="digest")`: `common_errors` is
/// InternalFailureException / InvalidRequestException / ThrottlingException). Named as the wire
/// code because a front-door refusal is rendered before any operation's error enum is reached.
pub const INVALID_REQUEST_EXCEPTION: &str = "InvalidRequestException";

/// The error code this plane reports a resource it cannot address with.
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";

/// Every error shape this service's model declares, with the status the model binds it to.
///
/// Source: `smithy_query(query="digest")` for the shapes each operation declares plus the
/// service-common ones, and `smithy_query(query="operation", operation="…")` for each entry's
/// `httpError`. It is the table [`modelled_status`] reads, and the set of bodies the `traceId`
/// member is completed on — a body naming a shape this service does not model (the framework's
/// own synthetic validation shape, the scaffold's 501) is left exactly as its renderer wrote
/// it.
const MODELLED_ERRORS: &[(&str, u16)] = &[
    ("ConflictException", 409),
    ("ForbiddenException", 403),
    ("GatewayTimeoutException", 504),
    ("InternalFailureException", 500),
    (INVALID_REQUEST_EXCEPTION, 400),
    ("MethodNotAllowedException", 405),
    ("RequestEntityTooLargeException", 413),
    (RESOURCE_NOT_FOUND_EXCEPTION, 404),
    ("ServiceUnavailableException", 503),
    ("ThrottlingException", 429),
    ("UnauthorizedException", 401),
    ("UnsupportedDocumentEncodingException", 415),
];

/// The member the connection operations bind the client id as, as a violation's path spells
/// it: the lowerCamel of the `@httpLabel` `clientId` on `GetConnection`, `ListSubscriptions`,
/// `SendDirectMessage` and `DeleteConnection`.
const CLIENT_ID_MEMBER: &str = "clientId";

/// The member the shadow operations bind the thing name as, as a violation's path spells it.
const THING_NAME_MEMBER: &str = "thingName";

/// The operations whose `thingName` is resolved to a thing and whose model gives them a
/// not-found error to report one they cannot resolve with.
///
/// Source: `smithy_query(query="digest")` — of the four operations binding a `thingName`
/// label, these three declare `ResourceNotFoundException` and `UpdateThingShadow` does not.
const THING_NOT_FOUND_OPERATIONS: &[&str] = &[
    "DeleteThingShadow",
    "GetThingShadow",
    "ListNamedShadowsForThing",
];

/// The lowest status a response carries an error envelope under; below it a response is a
/// success and carries no `message` for a `traceId` to sit beside.
const FIRST_ERROR_STATUS: u16 = 400;

/// The member every error envelope of this plane carries beside `message`.
const TRACE_ID_MEMBER: &str = "traceId";

/// The wrapper this service hands back from `Service::override_operation`: it serves the
/// operation the routing table names and then completes the envelope that walk rendered
/// ([`stated_envelope`]).
///
/// A `static` so the reference that seam returns outlives the call, and one value for the whole
/// process because the wrapper holds nothing per service but that one function.
pub static REFUSAL_ENVELOPE: RestatedRefusalOperation =
    RestatedRefusalOperation::new(stated_envelope);

/// How this service states a refusal the framework raised before any handler ran — the whole
/// `Service::override_refusal` body.
///
/// A refusal about one of the NAMES this plane addresses a resource by is stated in that name's
/// own sentence; every other one is the terse front-door sentence. A caller request
/// authentication refused is not a refusal about the request's values at all and keeps the
/// framework's own 403 code and wording: this service models `ForbiddenException` with that
/// status for exactly that case.
#[must_use]
pub fn stated_refusal(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    Some(
        addressed_name_refusal(violations, served_operation().as_deref()).unwrap_or(Refused {
            code: INVALID_REQUEST_EXCEPTION.to_string(),
            message: crate::state::BAD_REQUEST.to_string(),
        }),
    )
}

/// The refusal of a NAME this plane addresses a resource by, where every violation of the
/// refusal is about that one member, and `None` for anything else — a refusal even partly about
/// another member is the front door's, since these sentences each state ONE name's rules.
///
/// `operation` is the operation the request was routed to, which decides only whether the
/// thing-name refusal has a not-found error to be reported in (see the module docs).
fn addressed_name_refusal(violations: &[Violation], operation: Option<&str>) -> Option<Refused> {
    match sole_member(violations)? {
        CLIENT_ID_MEMBER => Some(Refused {
            code: INVALID_REQUEST_EXCEPTION.to_string(),
            message: crate::state::CLIENT_ID_NOT_VALID.to_string(),
        }),
        THING_NAME_MEMBER if operation.is_some_and(reports_an_absent_thing) => Some(Refused {
            code: RESOURCE_NOT_FOUND_EXCEPTION.to_string(),
            message: crate::state::INVALID_THING_NAME.to_string(),
        }),
        _ => None,
    }
}

/// The ONE member every violation of a refusal sits at, and `None` when they sit at more than
/// one or at none. The member is the last segment of a violation's path, which for a member of
/// the operation's own input is the whole path.
fn sole_member(violations: &[Violation]) -> Option<&str> {
    let mut members = violations
        .iter()
        .map(|violation| violation.path.split('.').next_back().unwrap_or_default());
    let first = members.next()?;
    members.all(|member| member == first).then_some(first)
}

/// Whether `operation` reports a thing name it cannot resolve as an absent resource
/// ([`THING_NOT_FOUND_OPERATIONS`]).
fn reports_an_absent_thing(operation: &str) -> bool {
    THING_NOT_FOUND_OPERATIONS.contains(&operation)
}

/// The [`::service_common::restated_refusal::Restatement`] [`REFUSAL_ENVELOPE`] answers
/// through: the envelope this plane writes every failure in, over whatever the walk rendered.
///
/// Neither the request nor the words noted at the refusal seam are read: the sentence is
/// already in the body the framework rendered from the code and message [`stated_refusal`]
/// chose, and both completions below are decided by the error SHAPE that body names.
fn stated_envelope(_req: &RtRequest, resp: RtResponse, _noted: Option<String>) -> RtResponse {
    with_modelled_status(with_trace_id(resp))
}

/// `resp` with the `traceId` member present on its error body.
///
/// Per MEMBER and non-destructive, through the one response-level rewrite `service_common`
/// states for every service whose wire carries a member the layer that rendered the body had no
/// value for: a body a handler serialized keeps the id that handler minted, so an envelope is
/// never given two ids and completing twice completes once. A success, a streamed body and a
/// body naming a shape this service does not model are returned byte for byte.
#[must_use]
fn with_trace_id(resp: RtResponse) -> RtResponse {
    if resp.status().as_u16() < FIRST_ERROR_STATUS {
        return resp;
    }
    let trace_id = crate::state::new_trace_id();
    ::service_common::error_detail_member::with_error_body_members(
        resp,
        &modelled_error_shapes(),
        &[(TRACE_ID_MEMBER, &trace_id)],
    )
}

/// `resp` under the status the model binds to the error shape it names, where that shape is one
/// this service models and the rendered status is not it.
///
/// Every header and every body byte the renderer wrote is kept: only the status line changes,
/// and a response naming no shape of this model — a success, the scaffold's 501 — is returned
/// as it was.
#[must_use]
fn with_modelled_status(resp: RtResponse) -> RtResponse {
    let named = resp
        .headers()
        .get(::service_common::error_type_locator::ERROR_TYPE_HEADER)
        .map(::service_common::error_type_locator::header_shape_named);
    let Some(status) = named.and_then(modelled_status) else {
        return resp;
    };
    if resp.status().as_u16() == status {
        return resp;
    }
    // An error body is built in memory by its renderer, so the bytes are readable here; a
    // streamed body is not an error body and keeps the status it was framed with.
    let Some(body) = resp.body().bytes().map(<[u8]>::to_vec) else {
        return resp;
    };
    let mut restated = RtResponse::new(
        StatusCode::try_from(status).expect("a status from this module's own table"),
        SdkBody::from(body),
    );
    for (name, value) in resp.headers().iter() {
        let _ = restated
            .headers_mut()
            .try_append(name.to_string(), value.to_string());
    }
    restated
}

/// The status the model binds to the error shape `named`, or `None` for a shape this service
/// does not model.
fn modelled_status(named: &str) -> Option<u16> {
    MODELLED_ERRORS
        .iter()
        .find(|(shape, _status)| *shape == named)
        .map(|(_shape, status)| *status)
}

/// The error shapes this service's model declares, by the wire code they render under — the
/// bodies the envelope completion above owns.
fn modelled_error_shapes() -> Vec<&'static str> {
    MODELLED_ERRORS.iter().map(|(shape, _)| *shape).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::aws_smithy_schema_server::constraints::ViolationKind;
    use ::serde_json::Value;

    /// A violation of `kind` at `path`, as the framework's constraint walk reports one.
    fn violation(path: &str, kind: ViolationKind) -> Violation {
        Violation {
            path: path.to_string(),
            kind,
            value: Some("whatever the caller sent".to_string()),
            sensitive: false,
        }
    }

    /// One refusal the framework raised, and the words this plane states it in.
    struct StatedCase {
        name: &'static str,
        violations: Vec<Violation>,
        /// The operation the request was routed to, as the wrapper's scope reports it.
        operation: Option<&'static str>,
        code: &'static str,
        message: &'static str,
    }

    /// RULE: a refusal about one of the names this plane addresses a resource by is stated in
    /// that name's own fixed sentence, under the class the model gives it; every other front
    /// door refusal is the terse sentence. The rows span the member, the constraint class
    /// (length, pattern, range, absence) and, for a thing name, the operation.
    #[test]
    fn each_addressed_name_is_refused_in_its_own_sentence() {
        let rows: Vec<StatedCase> = vec![
            StatedCase {
                name: "a client id outside its pattern",
                violations: vec![violation(
                    CLIENT_ID_MEMBER,
                    ViolationKind::Pattern("^[^$]".to_string()),
                )],
                operation: Some("GetConnection"),
                code: INVALID_REQUEST_EXCEPTION,
                message: crate::state::CLIENT_ID_NOT_VALID,
            },
            StatedCase {
                name: "a client id past its length",
                violations: vec![violation(CLIENT_ID_MEMBER, ViolationKind::MaxLength(128))],
                operation: Some("SendDirectMessage"),
                code: INVALID_REQUEST_EXCEPTION,
                message: crate::state::CLIENT_ID_NOT_VALID,
            },
            StatedCase {
                name: "a thing name outside its pattern on a read",
                violations: vec![violation(
                    THING_NAME_MEMBER,
                    ViolationKind::Pattern("^[a-zA-Z0-9:_-]+$".to_string()),
                )],
                operation: Some("GetThingShadow"),
                code: RESOURCE_NOT_FOUND_EXCEPTION,
                message: crate::state::INVALID_THING_NAME,
            },
            StatedCase {
                name: "a thing name past its length on a listing",
                violations: vec![violation(THING_NAME_MEMBER, ViolationKind::MaxLength(128))],
                operation: Some("ListNamedShadowsForThing"),
                code: RESOURCE_NOT_FOUND_EXCEPTION,
                message: crate::state::INVALID_THING_NAME,
            },
            StatedCase {
                name: "a thing name on a delete",
                violations: vec![violation(
                    THING_NAME_MEMBER,
                    ViolationKind::Pattern("^[a-zA-Z0-9:_-]+$".to_string()),
                )],
                operation: Some("DeleteThingShadow"),
                code: RESOURCE_NOT_FOUND_EXCEPTION,
                message: crate::state::INVALID_THING_NAME,
            },
            StatedCase {
                name: "a thing name on the operation with no not-found error",
                violations: vec![violation(THING_NAME_MEMBER, ViolationKind::MaxLength(128))],
                operation: Some("UpdateThingShadow"),
                code: INVALID_REQUEST_EXCEPTION,
                message: crate::state::BAD_REQUEST,
            },
            StatedCase {
                name: "a shadow name outside its pattern",
                violations: vec![violation(
                    "shadowName",
                    ViolationKind::Pattern("^[$a-zA-Z0-9:_-]+$".to_string()),
                )],
                operation: Some("GetThingShadow"),
                code: INVALID_REQUEST_EXCEPTION,
                message: crate::state::BAD_REQUEST,
            },
            StatedCase {
                name: "a page size outside its range",
                violations: vec![violation(
                    "pageSize",
                    ViolationKind::MaxValue("100".to_string()),
                )],
                operation: Some("ListNamedShadowsForThing"),
                code: INVALID_REQUEST_EXCEPTION,
                message: crate::state::BAD_REQUEST,
            },
            StatedCase {
                name: "a required member that was absent",
                violations: vec![violation("topic", ViolationKind::MissingRequiredMember)],
                operation: Some("SendDirectMessage"),
                code: INVALID_REQUEST_EXCEPTION,
                message: crate::state::BAD_REQUEST,
            },
            StatedCase {
                name: "a client id refused beside another member",
                violations: vec![
                    violation(CLIENT_ID_MEMBER, ViolationKind::MaxLength(128)),
                    violation("maxResults", ViolationKind::MaxValue("200".to_string())),
                ],
                operation: Some("ListSubscriptions"),
                code: INVALID_REQUEST_EXCEPTION,
                message: crate::state::BAD_REQUEST,
            },
            StatedCase {
                // The scope reports no operation only outside a served request, where there
                // is no not-found error to report a name in either.
                name: "a thing name with no operation in scope",
                violations: vec![violation(THING_NAME_MEMBER, ViolationKind::MaxLength(128))],
                operation: None,
                code: INVALID_REQUEST_EXCEPTION,
                message: crate::state::BAD_REQUEST,
            },
        ];

        for case in rows {
            let stated =
                addressed_name_refusal(&case.violations, case.operation).unwrap_or(Refused {
                    code: INVALID_REQUEST_EXCEPTION.to_string(),
                    message: crate::state::BAD_REQUEST.to_string(),
                });
            assert_eq!(
                stated.code, case.code,
                "case {:?}: expected the code {:?}, got {:?}",
                case.name, case.code, stated.code
            );
            assert_eq!(
                stated.message, case.message,
                "case {:?}: expected the sentence {:?}, got {:?}",
                case.name, case.message, stated.message
            );
            // No sentence of this front door carries anything of the request.
            assert!(
                !stated.message.contains("whatever the caller sent"),
                "case {:?}: the refusal echoes the caller's value: {:?}",
                case.name,
                stated.message
            );
        }
    }

    /// An authentication refusal is not about the request's values and keeps the framework's
    /// own words.
    #[test]
    fn an_authentication_refusal_is_left_to_the_framework() {
        assert!(
            stated_refusal(&Refusal::Unauthenticated).is_none(),
            "expected the framework's own 403 wording"
        );
    }

    /// A rendered refusal, as the framework's restJson1 renderer writes one.
    fn rendered(status: u16, shape: &str, body: &str) -> RtResponse {
        let mut resp = RtResponse::new(
            StatusCode::try_from(status).expect("a status the test states"),
            SdkBody::from(body.to_string()),
        );
        resp.headers_mut().insert("content-type", "application/json");
        resp.headers_mut()
            .insert("x-amzn-errortype", shape.to_string());
        resp
    }

    /// RULE: every error body this plane writes carries `traceId`, and a body that already
    /// carries one keeps it. The rows span the layer that rendered the body and the shape it
    /// names.
    #[test]
    fn every_error_envelope_carries_a_trace_id() {
        for (case, status, shape, body, already) in [
            (
                "a front-door refusal",
                400,
                INVALID_REQUEST_EXCEPTION,
                r#"{"__type":"InvalidRequestException","message":"Bad Request"}"#,
                None,
            ),
            (
                "a thing name reported absent",
                404,
                RESOURCE_NOT_FOUND_EXCEPTION,
                r#"{"__type":"ResourceNotFoundException","message":"Invalid thing name specified in request"}"#,
                None,
            ),
            (
                "a body a handler already completed",
                409,
                "ConflictException",
                r#"{"__type":"ConflictException","message":"Version conflict","traceId":"kept"}"#,
                Some("kept"),
            ),
        ] {
            let answered = stated_envelope(
                &RtRequest::new(SdkBody::empty()),
                rendered(status, shape, body),
                None,
            );
            let document: Value = ::serde_json::from_slice(
                answered.body().bytes().expect("an in-memory error body"),
            )
            .expect("a JSON error body");
            let trace_id = document
                .get(TRACE_ID_MEMBER)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert!(
                !trace_id.is_empty(),
                "case {case:?}: expected a traceId on the envelope, got {document:?}"
            );
            if let Some(kept) = already {
                assert_eq!(
                    trace_id, kept,
                    "case {case:?}: expected the id the handler minted, got {trace_id:?}"
                );
            }
        }
    }

    /// RULE: a refusal named in one of this service's own error shapes is answered under the
    /// status the model binds to that shape, whatever status the renderer wrote.
    #[test]
    fn a_refusal_carries_the_status_its_shape_is_modelled_with() {
        for (case, rendered_status, shape, expected) in [
            ("a not-found rendered at the refusal status", 400, RESOURCE_NOT_FOUND_EXCEPTION, 404),
            ("a bad request rendered at its own status", 400, INVALID_REQUEST_EXCEPTION, 400),
            ("an entity too large", 400, "RequestEntityTooLargeException", 413),
            ("a shape this service does not model", 400, "ValidationException", 400),
        ] {
            let answered = stated_envelope(
                &RtRequest::new(SdkBody::empty()),
                rendered(
                    rendered_status,
                    shape,
                    &format!(r#"{{"__type":"{shape}","message":"refused"}}"#),
                ),
                None,
            );
            assert_eq!(
                answered.status().as_u16(),
                expected,
                "case {case:?}: expected HTTP {expected}, got {}",
                answered.status().as_u16()
            );
            assert_eq!(
                answered
                    .headers()
                    .get("x-amzn-errortype")
                    .unwrap_or_default(),
                shape,
                "case {case:?}: expected the shape the renderer named to be kept"
            );
        }
    }
}
