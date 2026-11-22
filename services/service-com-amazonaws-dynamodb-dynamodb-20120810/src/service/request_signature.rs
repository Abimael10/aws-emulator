// SPDX-License-Identifier: CC0-1.0

//! How this endpoint answers a request whose SigV4 signature PRESENTATION is at fault.
//!
//! A request presents its signature in one of the two forms SigV4 defines — the
//! `Authorization` header, or the `X-Amz-*` query parameters a presigned request carries —
//! and the front door reads that presentation before any credential is resolved. What is
//! wrong with the PRESENTATION itself is a fault in the request, answered with the status a
//! request fault carries ([`FAULT_STATUS`]) and a class that names the fault:
//!
//! * nothing presents a signature at all — no `Authorization` header naming an AWS
//!   signature and no signature-bearing query string: the request is unsigned, and the
//!   answer names the missing token ([`MISSING_TOKEN`]);
//! * BOTH forms present one: the request names two auth mechanisms where exactly one is
//!   allowed, and the answer is the signature class this service names
//!   ([`super::AUTHENTICATION_REFUSAL`]) and NOTHING else. This one refusal states no
//!   sentence, which is not a gap but the MEASURED wire: the recorded answer to a request
//!   carrying an `Authorization` header beside `X-Amz-Algorithm` is the one-member document
//!   `{"__type": "<the signature class>"}`, with no `message` member for a client to read
//!   (the check this layer is pinned against compares that whole document, and says of the
//!   sentence that DynamoDB sends none for this refusal). The widely quoted
//!   one-auth-mechanism sentence is another AWS front door's wording for this class, not
//!   this endpoint's, so writing it here would state words this service does not send;
//!   `tests/all/signature_presentation_refusals.rs` pins the whole document instead.
//! * one form presents a signature with components MISSING: the answer names the
//!   incomplete-signature class ([`INCOMPLETE_SIGNATURE`]) and states one clause per
//!   component the request left out, so a caller reads which ones to add rather than being
//!   told its credential is unknown;
//! * the presentation is complete and the signature does not verify: the class and the
//!   sentence `service_common::authentication_refusal` owns for that failure, answered with
//!   the same status — what is at fault is the signature the request carried.
//!
//! The IDENTITY half is not this layer's: an access key id that resolves to no credential is
//! a fault of the CALLER rather than of the request's signature material, and keeps the
//! answer that layer states for it.
//!
//! # The two seams this layer is reached through
//!
//! The emulator verifies every signature itself, so three of the four faults above are
//! already a refusal by the time this service is asked anything: [`FaultStatedDefinition`]
//! wraps the definition's refusal renderer, which is the one seam handed both the refusal
//! and the request it refused.
//!
//! A request presenting TWO forms is the one fault a verifier that reads only one of them
//! cannot see — it reads the header, verifies it, and serves the request — so that one is
//! read ahead of every operation instead ([`two_forms_refusal`], wired into
//! `super::OperationScopedClauseForm`).
//!
//! Nothing here reads a credential, a secret or any stored state: a presentation is read
//! from the request's own headers and query string and nothing else. Nothing of the request
//! is echoed back either — every sentence above is fixed text — so a refusal cannot reflect
//! the caller's own `Authorization` header to whoever sent it.
//!
//! # Why this reads the presentation rather than calling the verifier's reader
//!
//! The framework's SigV4 verifier reads the same two forms
//! (`vendor/server/src/request_authentication.rs`, whose `authorization_field` and
//! `presigned_field` are private to that crate and reachable from no service), and it reads
//! them for the opposite question: whether there is a signature to RECOMPUTE. This layer is
//! only ever asked about a presentation that reader has already declined, so the two cannot
//! be the same function, and the differences are the point:
//!
//! * a header is a SigV4 presentation when it names the AWS4 FAMILY, not when it carries the
//!   one algorithm token the verifier requires — that is what tells `Authorization: AWS4-` (an
//!   incomplete SigV4 presentation) from `Authorization: X` (no AWS signature at all), which
//!   the recorded wire answers with two different classes; the components are then read after
//!   whichever token the header names, so a complete presentation naming another algorithm is
//!   complete here and goes to the verifier rather than being told it left out fields it sent;
//! * only the NAMES of the query parameters are compared, by the tree's own reader of a raw
//!   query string ([`::service_common::query_parameter`]), which compares them as written —
//!   a SigV4 signer spells them literally because they are part of the string it signed. The
//!   VALUES, which are what `presigned_field` percent-decodes, are never read here: this
//!   layer asks which components are present, never what they say.
//!
//! WHICH presentation a request carries, and the sentence an absent one is told, are not
//! DynamoDB's own: they are the same for every front door the emulator signs for, so they are
//! read from the module that owns them ([`::service_common::sigv4_presentation`]). What stays
//! here is what this endpoint alone decides — the classes it names, the status it answers a
//! request fault with, the components it lists as missing, and the media type it answers in.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use aws_smithy_schema_server::ProtocolError;
use aws_smithy_types::body::SdkBody;
use smithy_server_runtime::{
    ErasedServiceDefinition, OperationRoutingTable, Refused, ServerHttpProtocol, ServiceIdentity,
    WireProtocol,
};

/// The status a fault in the request's own signature presentation is answered with: the
/// request is malformed, which is the client's to fix, rather than a caller whose identity
/// this endpoint declines to serve.
const FAULT_STATUS: u16 = 400;

/// The namespace the endpoint's SigV4 layer names the shapes of its own refusals in — the
/// namespace [`super::front_door`] names a target it cannot bind in, and the one every
/// refusal this layer renders itself carries.
const CORAL_NAMESPACE: &str = "com.amazon.coral.service";

/// The class AWS's front door names a request that presents no signature at all, in that
/// namespace.
const MISSING_TOKEN: &str = "com.amazon.coral.service#MissingAuthenticationTokenException";

/// Its sentence, which is fixed: a request that carries no signature is told so, and nothing
/// of the request is echoed in it. The wording belongs to every front door the emulator signs
/// for rather than to this one, so it is read from the module that owns it
/// ([`::service_common::sigv4_presentation`]) — the sibling report
/// `issues/.../20261117T172826Z-dynamodb-5-net-new-causes-malformed-sigv4-answers.md` quotes
/// this endpoint's whole answer to an unsigned request, class and sentence together, and it is
/// that same sentence under the namespaced class above.
const MISSING_TOKEN_MESSAGE: &str =
    ::service_common::sigv4_presentation::MISSING_CREDENTIAL_MESSAGE;

/// The class AWS's front door names a presentation with components missing.
const INCOMPLETE_SIGNATURE: &str = "com.amazon.coral.service#IncompleteSignatureException";

/// The sentence that closes an incomplete QUERY-string presentation, after the clause for
/// each parameter the request left out.
const RE_EXAMINE_QUERY: &str = "Re-examine the query-string parameters.";

/// The header a request presents a signature in.
const AUTHORIZATION: &str = "authorization";

/// The fields a signature presented in the `Authorization` header must carry, in the order
/// the refusal lists the ones a request left out.
const HEADER_FIELDS: [&str; 3] = ["Credential", "Signature", "SignedHeaders"];

/// The clause that closes an incomplete header presentation when the request states no
/// instant either: a signature is computed over one, and either header may carry it.
const HEADER_DATE_CLAUSE: &str =
    "Authorization header requires existence of either a 'X-Amz-Date' or a 'Date' header.";

/// The two headers either of which states the instant a header-presented signature was
/// computed at.
const DATE_HEADERS: [&str; 2] = ["x-amz-date", "date"];

/// The query parameters a signature presented in the query string must carry, in the order
/// the refusal lists the ones a request left out.
///
/// The five that MAKE the presentation: the algorithm it names, the credential scope, the
/// instant, the headers that were signed, and the signature itself. `X-Amz-Expires` is not
/// among them — it bounds how long a complete presentation stays valid rather than making
/// one.
const QUERY_PARAMETERS: [&str; 5] = [
    "X-Amz-Algorithm",
    "X-Amz-Credential",
    "X-Amz-Date",
    "X-Amz-SignedHeaders",
    "X-Amz-Signature",
];

/// The response header a media type is announced in.
const CONTENT_TYPE: &str = "content-type";

/// What is wrong with the way a request presented its SigV4 signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureFault {
    /// Nothing presented a signature: no `Authorization` header naming an AWS signature and
    /// no signature in the query string.
    Absent,
    /// Both forms presented one, where exactly one may be used.
    TwoForms,
    /// One form presented a signature with components missing, listed in the order AWS
    /// states them, as the whole sentence the refusal carries.
    Incomplete(String),
    /// A complete presentation whose signature is not the one computed for the request,
    /// carrying the sentence the shared authentication layer owns for that failure.
    Unverified(String),
}

impl SignatureFault {
    /// The shape this fault's refusal names in its `__type`.
    ///
    /// Every class this layer renders is named in its own namespace ([`CORAL_NAMESPACE`]),
    /// which is the namespace the endpoint names the shapes of its own refusals in
    /// (`super::front_door`); the two signature faults share the one class this service
    /// names a signature failure by ([`super::AUTHENTICATION_REFUSAL`]).
    fn class(&self) -> String {
        match self {
            Self::Absent => MISSING_TOKEN.to_string(),
            Self::Incomplete(_) => INCOMPLETE_SIGNATURE.to_string(),
            // Both are about the SIGNATURE the request carried, which is the failure this
            // service names one code for (`super::AUTHENTICATION_REFUSAL`), in the namespace
            // this layer names every class it renders in.
            Self::TwoForms | Self::Unverified(_) => format!(
                "{CORAL_NAMESPACE}#{}",
                super::AUTHENTICATION_REFUSAL.signature_mismatch
            ),
        }
    }

    /// The sentence it carries, or `None` for the one refusal that carries none: the
    /// recorded answer to a request presenting two forms is the class alone, with no
    /// `message` member at all (see the module docs, which cite the measurement).
    fn message(&self) -> Option<&str> {
        match self {
            Self::Absent => Some(MISSING_TOKEN_MESSAGE),
            Self::TwoForms => None,
            Self::Incomplete(sentence) | Self::Unverified(sentence) => Some(sentence),
        }
    }
}

/// What is wrong with the way `req` presents its SigV4 signature, or `None` when it presents
/// exactly one complete signature — the one case a verifier can decide, because there is a
/// signature to recompute.
#[must_use]
pub fn presentation_fault(req: &RtRequest) -> Option<SignatureFault> {
    // WHICH of SigV4's presentations a request carries is one rule for every front door the
    // emulator signs for, so both halves are read through the module that owns it
    // ([`::service_common::sigv4_presentation`]): a header naming the AWS4 family presents a
    // signature, however incomplete the rest of it is, and one naming any other scheme
    // presents no AWS signature at all.
    let header = ::service_common::sigv4_presentation::presenting_authorization(
        req.headers().get(AUTHORIZATION),
    );
    let presents_in_query = ::service_common::sigv4_presentation::query_presents_a_credential(
        req.uri().split_once('?').map(|(_, query)| query),
    );
    match (header, presents_in_query) {
        (Some(_), true) => Some(SignatureFault::TwoForms),
        (None, false) => Some(SignatureFault::Absent),
        (Some(header), false) => incomplete_header(req, header),
        (None, true) => incomplete_query(req),
    }
}

/// The fault of a header presentation that leaves components out, or `None` when it carries
/// all of them.
///
/// One clause per component, each naming the component and asking for it, in the order the
/// clauses are listed. Nothing of the request is echoed: every clause is fixed text, and the
/// component names are this layer's own.
///
/// The fields are read after whichever algorithm token the header names, not after the one
/// this endpoint verifies: a complete presentation of another member of the AWS4 family
/// states every component, and saying it left them out would misreport what the request
/// carried. Such a presentation is complete here and is left to the verifier, which is what
/// decides whether the algorithm it names can be checked at all.
fn incomplete_header(req: &RtRequest, header: &str) -> Option<SignatureFault> {
    let fields = stated_fields(header);
    let mut clauses: Vec<String> = HEADER_FIELDS
        .iter()
        .filter(|field| header_field(fields, field).is_none())
        .map(|field| format!("Authorization header requires '{field}' parameter."))
        .collect();
    let states_an_instant = DATE_HEADERS
        .iter()
        .any(|name| req.headers().get(*name).is_some_and(|at| !at.is_empty()));
    if !states_an_instant {
        clauses.push(HEADER_DATE_CLAUSE.to_string());
    }
    if clauses.is_empty() {
        return None;
    }
    Some(SignatureFault::Incomplete(clauses.join(" ")))
}

/// The fault of a query-string presentation that leaves parameters out, or `None` when it
/// carries all of them.
fn incomplete_query(req: &RtRequest) -> Option<SignatureFault> {
    let mut clauses: Vec<String> = QUERY_PARAMETERS
        .iter()
        .filter(|name| query_parameter(req, name).is_none())
        .map(|name| format!("AWS query-string parameters must include '{name}'."))
        .collect();
    if clauses.is_empty() {
        return None;
    }
    clauses.push(RE_EXAMINE_QUERY.to_string());
    Some(SignatureFault::Incomplete(clauses.join(" ")))
}

/// The field list of a header presentation: everything after the algorithm token it names,
/// which a SigV4 presentation separates from the fields with one space. A header that names
/// a token and nothing else states no field at all.
fn stated_fields(header: &str) -> &str {
    header.split_once(' ').map_or("", |(_, fields)| fields)
}

/// The value of one field of a SigV4 `Authorization` header (`Credential=…`), read off the
/// comma-separated list [`stated_fields`] answers.
///
/// `None` for a field the list does not state, or states empty: a presentation that names a
/// component without a value has not presented it.
fn header_field<'h>(fields: &'h str, field: &str) -> Option<&'h str> {
    let value = fields.split(',').find_map(|stated| {
        let stated = stated.trim();
        stated.strip_prefix(field)?.strip_prefix('=')
    })?;
    (!value.is_empty()).then_some(value)
}

/// The value of one query parameter of `req`, or `None` for a parameter the query does not
/// carry, carries empty, or carries as a bare name with no value: a component named without
/// a value has not been presented.
///
/// The scan itself is the tree's one reader of a raw query string
/// ([`::service_common::query_parameter`]), which compares the name as written — a SigV4
/// signer spells these names literally, because they are part of the string it signed.
fn query_parameter<'r>(req: &'r RtRequest, name: &str) -> Option<&'r str> {
    let query = req.uri().split_once('?').map(|(_, query)| query)?;
    ::service_common::query_parameter(query, name).filter(|value| !value.is_empty())
}

/// The response that refuses `req` for `fault`: the class and its sentence, in this
/// endpoint's own JSON envelope, announced in the media type the request named the API
/// under.
///
/// The body is written through the JSON writer rather than as a literal, so a sentence that
/// echoes part of the request cannot break the document.
fn fault_response(req: &RtRequest, fault: &SignatureFault) -> RtResponse {
    let mut body = ::serde_json::Map::new();
    body.insert("__type".to_string(), fault.class().into());
    if let Some(message) = fault.message() {
        body.insert("message".to_string(), message.into());
    }
    let mut resp = RtResponse::new(
        StatusCode::try_from(FAULT_STATUS).expect("the status is a constant of this module"),
        SdkBody::from(::serde_json::Value::Object(body).to_string()),
    );
    resp.headers_mut()
        .insert(CONTENT_TYPE, super::front_door::PROTOCOL_MEDIA_TYPE);
    super::front_door::announced(req, resp)
}

/// The refusal a request presenting its signature in BOTH forms is answered with, or `None`
/// for a request that presents it in one form or in none.
///
/// This is the fault the emulator's verifier cannot see — it reads one form, verifies it and
/// serves the request — so it is read ahead of every operation
/// (`super::OperationScopedClauseForm`). The other faults do not reach that layer when
/// signatures are verified: the verifier has already refused them, and
/// [`FaultStatedDefinition`] words those.
#[must_use]
pub fn two_forms_refusal(req: &RtRequest) -> Option<RtResponse> {
    match presentation_fault(req) {
        Some(SignatureFault::TwoForms) => Some(fault_response(req, &SignatureFault::TwoForms)),
        _ => None,
    }
}

/// How this service words the refusal the authentication layer reported for `req`, when what
/// is at fault is the request's own signature material; `None` leaves the refusal to the
/// layer below, which words the IDENTITY failures.
fn stated_fault(req: &RtRequest, reported: &str) -> Option<RtResponse> {
    if let Some(fault) = presentation_fault(req) {
        return Some(fault_response(req, &fault));
    }
    // A complete presentation the verifier recomputed and did not match: the class and the
    // sentence the shared layer owns for that failure
    // (`service_common::authentication_refusal`), restated here only in the STATUS — a
    // signature that does not match is a fault in the request that carried it. What the
    // request presented is read through that same layer, so one reading answers for both.
    let presented = ::service_common::authentication_refusal::presented_credential(req);
    let stated: Refused = super::AUTHENTICATION_REFUSAL.stated(reported, presented)?;
    (stated.code == super::AUTHENTICATION_REFUSAL.signature_mismatch)
        .then(|| fault_response(req, &SignatureFault::Unverified(stated.message)))
}

/// A service's model-derived half with a refusal about the request's SIGNATURE MATERIAL
/// stated as this endpoint states it (see the module docs).
///
/// Identity, the routing table, the protocol and every other rendering come from the wrapped
/// definition untouched. Generic over that definition rather than over an engine, so this
/// module names no generated type.
pub struct FaultStatedDefinition<D> {
    inner: D,
}

impl<D: ErasedServiceDefinition> FaultStatedDefinition<D> {
    /// Wrap a definition, stating a signature fault the way this endpoint states it.
    pub const fn new(inner: D) -> Self {
        Self { inner }
    }
}

impl<D: ErasedServiceDefinition> ErasedServiceDefinition for FaultStatedDefinition<D> {
    fn identity(&self) -> &ServiceIdentity {
        self.inner.identity()
    }

    fn protocol(&self) -> &dyn ServerHttpProtocol {
        self.inner.protocol()
    }

    fn routing_table(&self) -> &OperationRoutingTable {
        self.inner.routing_table()
    }

    fn routing_error_response(&self, req: &RtRequest) -> RtResponse {
        self.inner.routing_error_response(req)
    }

    fn framework_error_response(&self, error: ProtocolError, req: &RtRequest) -> RtResponse {
        self.inner.framework_error_response(error, req)
    }

    fn refusal_response(
        &self,
        error: ProtocolError,
        chosen: Option<Refused>,
        req: &RtRequest,
    ) -> RtResponse {
        // A refusal the service worded itself is passed through: this layer answers the
        // authentication layer's own verdict, and only where the request's signature
        // material is what failed.
        if let (None, ProtocolError::Unauthenticated { code, .. }) = (&chosen, &error) {
            if let Some(stated) = stated_fault(req, code) {
                return stated;
            }
        }
        self.inner.refusal_response(error, chosen, req)
    }

    fn wire_protocol(&self, req: &RtRequest) -> WireProtocol<'_> {
        self.inner.wire_protocol(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The instant a dated request below states.
    const SIGNED_AT: &str = "20260101T000000Z";

    /// The class a refusal about the SIGNATURE a request carried names, written out here
    /// rather than composed the way the code composes it, so the rows state the whole wire
    /// value a client reads.
    const SIGNATURE_CLASS: &str = "com.amazon.coral.service#InvalidSignatureException";

    /// A complete `Authorization` header: the algorithm token and the three fields a header
    /// presentation carries.
    const COMPLETE_HEADER: &str = "AWS4-HMAC-SHA256 \
                                   Credential=AKIDTEST/20260101/us-east-1/dynamodb/aws4_request, \
                                   SignedHeaders=host;x-amz-date, Signature=abc";

    /// The query string of a complete presigned presentation.
    const COMPLETE_QUERY: &str = "X-Amz-Algorithm=AWS4-HMAC-SHA256\
                                  &X-Amz-Credential=AKIDTEST%2F20260101%2Fus-east-1%2Fdynamodb\
                                  &X-Amz-Date=20260101T000000Z&X-Amz-Expires=900\
                                  &X-Amz-SignedHeaders=host&X-Amz-Signature=abc";

    /// One request: the `Authorization` header it carries, the query string it was sent
    /// with, and whether it states an instant in a header.
    fn request(header: Option<&str>, query: Option<&str>, dated: bool) -> RtRequest {
        let mut req = RtRequest::new(SdkBody::empty());
        req.set_method("POST").expect("a valid method");
        req.set_uri(match query {
            Some(query) => format!("/?{query}"),
            None => "/".to_string(),
        })
        .expect("a valid uri");
        if let Some(value) = header {
            req.headers_mut().insert(AUTHORIZATION, value.to_string());
        }
        if dated {
            req.headers_mut().insert("x-amz-date", SIGNED_AT);
        }
        req
    }

    /// One row of the table below: the case, the `Authorization` header the request carries,
    /// the query string it is sent with, whether it states an instant, and the class and
    /// sentence the fault states (`None` for a presentation that faults in neither way, and
    /// a class with no sentence for the refusal that carries none).
    type Row = (
        &'static str,
        Option<&'static str>,
        Option<&'static str>,
        bool,
        Option<(&'static str, Option<&'static str>)>,
    );

    /// Which fault each way of presenting a signature is: the class and the whole sentence,
    /// over the forms a request can arrive in.
    #[test]
    fn each_presentation_is_the_fault_it_states() {
        // (case, authorization, query, states an instant, the class and sentence expected)
        let rows: [Row; 13] = [
            (
                "a complete header presentation, which only a verifier can decide",
                Some(COMPLETE_HEADER),
                None,
                true,
                None,
            ),
            (
                "a complete presentation naming another member of the AWS4 family: complete \
                 here too, and the verifier's to decide",
                Some(
                    "AWS4-ECDSA-P256-SHA256 \
                     Credential=AKIDTEST/20260101/us-east-1/dynamodb/aws4_request, \
                     SignedHeaders=host;x-amz-date, Signature=abc",
                ),
                None,
                true,
                None,
            ),
            (
                "a complete query presentation, likewise",
                None,
                Some(COMPLETE_QUERY),
                false,
                None,
            ),
            (
                "no presentation at all",
                None,
                None,
                false,
                Some((MISSING_TOKEN, Some(MISSING_TOKEN_MESSAGE))),
            ),
            (
                "a header of another authentication scheme, which presents no AWS signature",
                Some("Bearer 0123456789"),
                None,
                false,
                Some((MISSING_TOKEN, Some(MISSING_TOKEN_MESSAGE))),
            ),
            (
                "an empty header, which presents nothing either",
                Some(""),
                None,
                false,
                Some((MISSING_TOKEN, Some(MISSING_TOKEN_MESSAGE))),
            ),
            (
                "a query string carrying a signature but not the algorithm that names the \
                 mechanism, which presents none either",
                None,
                Some("X-Amz-Signature=abc&X-Amz-SignedHeaders=host"),
                false,
                Some((MISSING_TOKEN, Some(MISSING_TOKEN_MESSAGE))),
            ),
            (
                "a query string naming the algorithm with no value at all, which states no \
                 mechanism either",
                None,
                Some("X-Amz-Algorithm&X-Amz-Signature=abc"),
                false,
                Some((MISSING_TOKEN, Some(MISSING_TOKEN_MESSAGE))),
            ),
            (
                "a bare algorithm family token: every component missing, the instant too",
                Some("AWS4-"),
                None,
                false,
                Some((
                    INCOMPLETE_SIGNATURE,
                    Some(
                        "Authorization header requires 'Credential' parameter. Authorization \
                         header requires 'Signature' parameter. Authorization header \
                         requires 'SignedHeaders' parameter. Authorization header requires \
                         existence of either a 'X-Amz-Date' or a 'Date' header.",
                    ),
                )),
            ),
            (
                "a header stating its credential scope alone, dated",
                Some(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDTEST/20260101/us-east-1/dynamodb/aws4_request",
                ),
                None,
                true,
                Some((
                    INCOMPLETE_SIGNATURE,
                    Some(
                        "Authorization header requires 'Signature' parameter. Authorization \
                         header requires 'SignedHeaders' parameter.",
                    ),
                )),
            ),
            (
                "a query presentation missing the headers it signed",
                None,
                Some(
                    "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIDTEST%2Fscope\
                     &X-Amz-Date=20260101T000000Z&X-Amz-Signature=abc",
                ),
                false,
                Some((
                    INCOMPLETE_SIGNATURE,
                    Some(
                        "AWS query-string parameters must include 'X-Amz-SignedHeaders'. \
                         Re-examine the query-string parameters.",
                    ),
                )),
            ),
            (
                "a query presentation naming only its algorithm",
                None,
                Some("X-Amz-Algorithm=AWS4-HMAC-SHA256"),
                false,
                Some((
                    INCOMPLETE_SIGNATURE,
                    Some(
                        "AWS query-string parameters must include 'X-Amz-Credential'. AWS \
                         query-string parameters must include 'X-Amz-Date'. AWS query-string \
                         parameters must include 'X-Amz-SignedHeaders'. AWS query-string \
                         parameters must include 'X-Amz-Signature'. Re-examine the \
                         query-string parameters.",
                    ),
                )),
            ),
            (
                "both forms at once, which is one auth mechanism too many: the class alone",
                Some(COMPLETE_HEADER),
                Some(COMPLETE_QUERY),
                true,
                Some((SIGNATURE_CLASS, None)),
            ),
        ];

        for (case, header, query, dated, expected) in rows {
            let fault = presentation_fault(&request(header, query, dated));
            let stated = fault.as_ref().map(|fault| (fault.class(), fault.message()));
            let actual = stated
                .as_ref()
                .map(|(class, message)| (class.as_str(), *message));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// Only the two-forms fault is read ahead of an operation: every other presentation
    /// reaches the verifier, which is what decides whether the request is served.
    #[test]
    fn the_gate_ahead_of_every_operation_reads_the_two_forms_fault_alone() {
        // (case, authorization, query, refused ahead of the operation)
        let rows: [(&str, Option<&str>, Option<&str>, bool); 4] = [
            (
                "both forms",
                Some(COMPLETE_HEADER),
                Some(COMPLETE_QUERY),
                true,
            ),
            ("the header alone", Some(COMPLETE_HEADER), None, false),
            ("the query alone", None, Some(COMPLETE_QUERY), false),
            ("neither, as an unsigned request arrives", None, None, false),
        ];

        for (case, header, query, expected) in rows {
            let refused = two_forms_refusal(&request(header, query, true)).is_some();
            assert_eq!(
                refused, expected,
                "case {case:?}: expected refused={expected}, got {refused}"
            );
        }
    }

    /// Every fault is answered with the status a request fault carries, in the endpoint's own
    /// JSON envelope.
    #[test]
    fn a_fault_is_answered_as_a_fault_in_the_request() {
        let unsigned = request(None, None, false);
        let fault = presentation_fault(&unsigned).expect("an unsigned request faults");
        let resp = fault_response(&unsigned, &fault);
        assert_eq!(
            resp.status().as_u16(),
            FAULT_STATUS,
            "case an unsigned request: expected HTTP {FAULT_STATUS}, got {}",
            resp.status().as_u16()
        );
        let body = String::from_utf8_lossy(resp.body().bytes().unwrap_or_default()).to_string();
        let document: ::serde_json::Value =
            ::serde_json::from_str(&body).expect("the refusal is a JSON document");
        assert_eq!(
            (
                document["__type"].as_str().unwrap_or_default(),
                document["message"].as_str().unwrap_or_default(),
            ),
            (MISSING_TOKEN, MISSING_TOKEN_MESSAGE),
            "case an unsigned request: expected the missing-token document, got {body}"
        );
        // And the one refusal that states no sentence states exactly that: the class, and no
        // `message` member at all.
        let two_forms = fault_response(&unsigned, &SignatureFault::TwoForms);
        let body =
            String::from_utf8_lossy(two_forms.body().bytes().unwrap_or_default()).to_string();
        assert_eq!(
            body,
            format!("{{\"__type\":\"{SIGNATURE_CLASS}\"}}"),
            "case two forms: expected the class as the whole body, got {body}"
        );
    }
}
