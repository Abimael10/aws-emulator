// SPDX-License-Identifier: CC0-1.0

//! How an operation that validates its own page size refuses one outside the range its
//! member declares.
//!
//! Every paginated input of this model carries its page size in one member, `MaxResults`, an
//! `Integer` with `@range {min: 1, max: …}` — and it is the only member of the whole model
//! that declares a range at all
//! (`smithy_query(query="shape",shape="ListSupportedResourceTypesInput,ListResourcesInput,SearchInput,ListIndexesInput,ListViewsInput,ListManagedViewsInput,ListServiceViewsInput,ListServiceIndexesInput,ListIndexesForMembersInput,ListStreamingAccessForServicesInput,GetResourceExplorerSetupInput")`,
//! whose maxima differ per operation while the minimum is 1 on every one of them).
//!
//! # The two answers, and which operation gives which
//!
//! A request BODY this service refuses is normally refused ahead of the code that owns its
//! modelled errors, with one fixed sentence that names neither the member nor the value
//! ([`service_common::front_door::INVALID_REQUEST_BODY`]) — which is the recorded answer to a
//! page size past `ListResources`'s own maximum.
//!
//! An operation that validates its page size ITSELF answers the same modelled
//! `ValidationException` (HTTP 400) with a sentence that names the member and states the
//! bound the value broke, plus a `FieldList` carrying one `ValidationExceptionField` naming
//! that member (`$Name`) and the bound (`$ValidationIssue`) — both `@required` on that shape
//! (`smithy_query(query="shape",shape="ValidationException,ValidationExceptionField")`).
//! Neither sentence states the value the request submitted, so one answer serves every value
//! on the refused side of a bound.
//!
//! Which operation validates its own page size is a fact about the SERVICE, read off its
//! recorded answers one operation at a time, and [`OWN_PAGE_SIZE_VALIDATION`] is that list:
//! an operation not on it keeps the fixed sentence, as `ListResources`'s recording shows for
//! that operation. The bounds in the sentence and in the field entry are never restated here:
//! they come from the violation, which carries the member's own declared bound.
//!
//! # Why the body is written here
//!
//! A refusal reached before any handler runs is worded through the framework's refusal
//! channel, which carries a code and a message ([`Refused`]) and nothing else — so it can
//! carry neither `FieldList`, a MEMBER of the modelled shape a client reads the offending
//! member's name out of, nor a sentence chosen per operation, because that channel is handed
//! the violations alone. [`PageSizeRefusalDefinition`] wraps this service's generated
//! definition at the one seam a rendered refusal passes through
//! ([`ErasedServiceDefinition::refusal_response`], which this service's front door calls),
//! where the REQUEST is in hand and so the operation is too: it writes the modelled members
//! over the body the framework's renderer built, keeping that renderer's status and its
//! `x-amzn-errortype` header. It is the sibling of
//! [`service_common::front_door::UnauthorizedDefinition`], which answers at the same seam the
//! one verdict a wording seam cannot state.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema_server::constraints::{Violation, ViolationKind};
use aws_smithy_schema_server::ProtocolError;
use aws_smithy_types::body::SdkBody;
use smithy_server_runtime::{
    ErasedServiceDefinition, OperationRoutingTable, Refused, ServerHttpProtocol, ServiceIdentity,
    WireProtocol,
};

/// The operations whose own validation refuses their page size, and so answer the member and
/// the bound rather than this service's fixed body sentence.
///
/// One entry per operation whose recorded refusal shows that answer. `ListResources` is
/// recorded with the fixed sentence instead, so an operation this list does not name keeps it.
const OWN_PAGE_SIZE_VALIDATION: &[&str] = &["ListSupportedResourceTypes"];

/// The member every paginated input of this model carries its page size in, spelled the way
/// the MODEL names it — which is how AWS reports it, in the sentence and in
/// `ValidationExceptionField$Name`, whatever casing the validating walk's violation path
/// uses for it.
const PAGE_SIZE_MEMBER: &str = "MaxResults";

/// `ValidationException$Message`, as the model names it.
const MESSAGE_MEMBER: &str = "Message";

/// `ValidationException$FieldList`, and the two `@required` members of each entry.
const FIELD_LIST_MEMBER: &str = "FieldList";
const FIELD_NAME_MEMBER: &str = "Name";
const FIELD_ISSUE_MEMBER: &str = "ValidationIssue";

/// Which end of its declared range a page size fell outside, and the bound it broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageSizeFault {
    /// Below the declared minimum.
    Below(i64),
    /// Above the declared maximum.
    Above(i64),
}

impl PageSizeFault {
    /// What the service says about this fault: the member at fault and the bound, with
    /// nothing from the request in it.
    ///
    /// Below the minimum the sentence states the greatest value the bound refuses; above the
    /// maximum it states the greatest value the bound admits.
    fn message(self) -> String {
        match self {
            Self::Below(minimum) => format!(
                "{PAGE_SIZE_MEMBER} must be greater than {}.",
                minimum.saturating_sub(1)
            ),
            Self::Above(maximum) => format!("{PAGE_SIZE_MEMBER} cannot exceed {maximum}."),
        }
    }

    /// The bound the value broke, as `ValidationExceptionField$ValidationIssue` states it:
    /// the relation the submitted value stands in to the bound it broke.
    fn issue(self) -> String {
        match self {
            Self::Below(minimum) => format!("< {minimum}"),
            Self::Above(maximum) => format!("> {maximum}"),
        }
    }
}

/// Whether `path` names the page-size member.
///
/// The validating walk spells a member in lowerCamel, so the comparison is
/// case-insensitive rather than a second spelling of the name kept in step by hand.
fn is_page_size(path: &str) -> bool {
    path.eq_ignore_ascii_case(PAGE_SIZE_MEMBER)
}

/// Every page-size fault `violations` reports, in the order the walk found them. Empty when
/// none of them is about the page size.
///
/// The bound comes from the violation, which carries the model's own declared bound for the
/// member that broke it, so no bound is restated here.
fn page_size_faults(violations: &[Violation]) -> Vec<PageSizeFault> {
    violations
        .iter()
        .filter(|violation| is_page_size(&violation.path))
        .filter_map(|violation| match &violation.kind {
            ViolationKind::MinValue(minimum) => {
                minimum.parse::<i64>().ok().map(PageSizeFault::Below)
            }
            ViolationKind::MaxValue(maximum) => {
                maximum.parse::<i64>().ok().map(PageSizeFault::Above)
            }
            _ => None,
        })
        .collect()
}

/// The modelled `ValidationException` body for the page-size faults `faults`: the sentence
/// for the first of them, and one `FieldList` entry per fault naming the member and the
/// bound its value broke.
///
/// `restJson1` conveys the error SHAPE in the `x-amzn-errortype` header, so the body carries
/// the shape's own modelled members and nothing else.
fn refusal_body(faults: &[PageSizeFault]) -> Option<Vec<u8>> {
    let message = faults.first()?.message();
    let fields: Vec<::serde_json::Value> = faults
        .iter()
        .map(|fault| {
            ::serde_json::json!({
                FIELD_NAME_MEMBER: PAGE_SIZE_MEMBER,
                FIELD_ISSUE_MEMBER: fault.issue(),
            })
        })
        .collect();
    ::serde_json::to_vec(&::serde_json::json!({
        MESSAGE_MEMBER: message,
        FIELD_LIST_MEMBER: fields,
    }))
    .ok()
}

/// `resp` carrying `body`, with the advertised length corrected when the renderer advertised
/// one at all.
fn with_body(mut resp: RtResponse, body: Vec<u8>) -> RtResponse {
    if resp.headers().get("content-length").is_some() {
        let length = body.len().to_string();
        // A decimal length is always a valid header value, so this cannot fail today; if it
        // ever did, the header is REMOVED rather than left describing the body the renderer
        // wrote, so the two can never disagree.
        if resp
            .headers_mut()
            .try_insert("content-length", length)
            .is_err()
        {
            resp.headers_mut().remove("content-length");
        }
    }
    *resp.body_mut() = SdkBody::from(body);
    resp
}

/// This service's generated definition, answering a page size refused by an operation that
/// validates its own with the members that operation's modelled `ValidationException`
/// declares (see the module docs).
///
/// Identity, the routing table, the protocol and every other rendering come from the wrapped
/// definition untouched — nothing model-derived is restated here. Generic over that
/// definition rather than over an engine, so this module names no generated type.
pub struct PageSizeRefusalDefinition<D> {
    inner: D,
}

impl<D: ErasedServiceDefinition> PageSizeRefusalDefinition<D> {
    /// Wrap this service's generated definition.
    pub fn new(inner: D) -> Self {
        Self { inner }
    }

    /// The modelled body this refusal of `req` carries, or `None` to leave the rendered
    /// refusal exactly as the framework wrote it.
    ///
    /// The operation is the one the request names, resolved through the generated
    /// definition's own protocol — the same key the routing table is read by — so no second
    /// reading of a request's route can drift from it.
    fn modelled_body(&self, error: &ProtocolError, req: &RtRequest) -> Option<Vec<u8>> {
        let ProtocolError::Invalid(violations) = error else {
            return None;
        };
        let operation = self.inner.protocol().routing_key(req).ok()?;
        if !OWN_PAGE_SIZE_VALIDATION.contains(&operation) {
            return None;
        }
        refusal_body(&page_size_faults(violations))
    }
}

impl<D: ErasedServiceDefinition> ErasedServiceDefinition for PageSizeRefusalDefinition<D> {
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
        // Read before the renderer consumes the error, and only for the one class: every
        // other refusal — a body that breaks another of the model's rules, a page size of an
        // operation that does not validate its own, a value bound to the request URI, an
        // unverified caller — is answered exactly as it was.
        let body = self.modelled_body(&error, req);
        let answered = self.inner.refusal_response(error, chosen, req);
        match body {
            Some(body) => with_body(answered, body),
            None => answered,
        }
    }

    fn wire_protocol(&self, req: &RtRequest) -> WireProtocol<'_> {
        self.inner.wire_protocol(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One violation of `kind` at `path`, as the validating walk reports it.
    fn violation(path: &str, kind: ViolationKind) -> Violation {
        Violation {
            path: path.to_string(),
            kind,
            value: Some("0".to_string()),
            sensitive: false,
        }
    }

    /// RULE: the body of this refusal carries the modelled members — the sentence naming the
    /// member and the bound its value broke, and one `FieldList` entry per refused member —
    /// and nothing else; a violation of any other member or class has no body of this shape.
    ///
    /// The rows differ along what the rule generalises over: which end of the range the value
    /// fell outside, the magnitude of the declared bound, the casing the walk spells the
    /// member in, and the class of the violation.
    #[test]
    fn a_refused_page_size_is_reported_with_the_member_and_the_bound() {
        // (case, the violations, the body the refusal carries)
        let rows: Vec<(&str, Vec<Violation>, Option<::serde_json::Value>)> = vec![
            (
                "below the minimum of 1, the bound every paginated input declares",
                vec![violation("maxResults", ViolationKind::MinValue("1".to_string()))],
                Some(::serde_json::json!({
                    "Message": "MaxResults must be greater than 0.",
                    "FieldList": [{ "Name": "MaxResults", "ValidationIssue": "< 1" }],
                })),
            ),
            (
                "above a maximum of 1000",
                vec![violation(
                    "maxResults",
                    ViolationKind::MaxValue("1000".to_string()),
                )],
                Some(::serde_json::json!({
                    "Message": "MaxResults cannot exceed 1000.",
                    "FieldList": [{ "Name": "MaxResults", "ValidationIssue": "> 1000" }],
                })),
            ),
            (
                "above a maximum of 50, another operation's own bound",
                vec![violation(
                    "maxResults",
                    ViolationKind::MaxValue("50".to_string()),
                )],
                Some(::serde_json::json!({
                    "Message": "MaxResults cannot exceed 50.",
                    "FieldList": [{ "Name": "MaxResults", "ValidationIssue": "> 50" }],
                })),
            ),
            (
                "the member spelled as the model names it",
                vec![violation("MaxResults", ViolationKind::MinValue("1".to_string()))],
                Some(::serde_json::json!({
                    "Message": "MaxResults must be greater than 0.",
                    "FieldList": [{ "Name": "MaxResults", "ValidationIssue": "< 1" }],
                })),
            ),
            (
                "another member below its own minimum length",
                vec![violation("nextToken", ViolationKind::MinLength(1))],
                None,
            ),
            (
                "a required member the request omits",
                vec![violation(
                    "queryString",
                    ViolationKind::MissingRequiredMember,
                )],
                None,
            ),
        ];

        for (case, violations, expected) in rows {
            let body = refusal_body(&page_size_faults(&violations));
            let document = body.map(|bytes| {
                ::serde_json::from_slice::<::serde_json::Value>(&bytes)
                    .unwrap_or_else(|e| panic!("case {case:?}: the body must be JSON ({e})"))
            });
            assert_eq!(
                document, expected,
                "case {case:?}: expected the body {expected:?}, got {document:?}"
            );
        }
    }
}
