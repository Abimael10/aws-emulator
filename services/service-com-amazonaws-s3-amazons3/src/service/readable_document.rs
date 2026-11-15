// SPDX-License-Identifier: CC0-1.0

//! The refusal a request DOCUMENT earns when the schema this API publishes for it cannot
//! READ the document at all.
//!
//! # What this covers
//!
//! A document whose members are all readable and whose values break a declared constraint is
//! refused by the validating walk, and this service words that refusal itself
//! (`crate::service::s3_refusal`: a value refused inside the request's XML document is
//! `MalformedXML`, "the XML you provided was not well-formed or did not validate against our
//! published schema"). A document carrying a value of the wrong SHAPE for the member it was
//! written for — a boolean spelled outside the lexical forms XML gives one, say — never
//! reaches that walk: the codec fails while BUILDING the input, which the framework answers
//! with its own generic code and the codec's own sentence about Rust types.
//!
//! To a caller both are one fault: the document did not validate against the published
//! schema, which is the one refusal S3 answers a payload it cannot take with. So an
//! operation wrapped in [`ReadableDocument`] answers that same refusal
//! ([`crate::state::MALFORMED_XML_CODE`], under this service's own envelope) for a document
//! its published schema cannot read, before the operation behind it runs.
//!
//! # Why a wrapper
//!
//! The framework hands a codec failure to no service seam: its refusal hook
//! (`smithy_server_runtime::Service::override_refusal`) is reached for an absent required
//! member and for a value outside a declared bound, and the codec's own failure renders
//! straight from its message. An operation wrapper is the one place ahead of that sequence,
//! and it is per operation — which is what keeps the document rule with the operations whose
//! payload rules this service states, rather than rewording every codec failure of every
//! operation at once.
//!
//! An ABSENT required member is left alone here: the walk finds it (the framework folds it
//! into exactly the violation the walk reports), so it keeps whatever wording the operation's
//! own door or this service's refusal seam gives it.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema::serde::SerdeError;
use aws_smithy_schema_server::ProtocolError;
use smithy_server_runtime::{
    BoxFuture, DeserializableStruct, ErasedOperation, Operation, OperationContext, Refused,
    Service,
};

/// One operation served with ONE refusal in front of it: a request document the published
/// schema cannot read.
///
/// `Served` is what actually serves the request — the operation, or the operation already
/// wrapped in its own front door — and `Judged` is the modelled operation whose input schema
/// the document is read against. They are the same operation; two parameters because a door
/// is an erased operation and only the modelled one carries the schema and the input type.
pub struct ReadableDocument<Judged, Served> {
    served: Served,
    judged: ::std::marker::PhantomData<fn() -> Judged>,
}

impl<Judged, Served> ReadableDocument<Judged, Served> {
    /// Wrap what serves the request, judging its document against `Judged`'s input schema.
    pub fn new(served: Served) -> Self {
        Self {
            served,
            judged: ::std::marker::PhantomData,
        }
    }
}

impl<Judged, Served> ErasedOperation for ReadableDocument<Judged, Served>
where
    Judged: Operation,
    Served: ErasedOperation,
{
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // The answer is computed in one call that reads, uses and drops the protocol:
            // `&dyn ErasedServiceDefinition` is not `Sync`, so holding it across the await
            // below would make this future non-`Send` (the reading the shared front door
            // gives its own refusal).
            if let Some(response) = self.refusal(svc, req) {
                return response;
            }
            self.served.run(svc, req, ctx).await
        })
    }

    /// The wrapped operation's modelled auth schemes: wrapping an operation must not change
    /// which schemes sign it.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        self.served.auth_schemes()
    }
}

impl<Judged: Operation, Served> ReadableDocument<Judged, Served> {
    /// This service's answer to a request whose document the published schema cannot read,
    /// or `None` when it can read it — in which case what is wrapped serves the request.
    ///
    /// The read is the framework's own, through the codec the request is SERVED in (the
    /// author's `override_protocol` when the service replaces the model-derived one, else the
    /// definition's), so this cannot judge the document by a different reading from the
    /// sequence that follows it. The cost is a second parse of the same bytes, bounded by the
    /// body limits the framework already enforces — the same cost the shared front door pays
    /// for the same reason.
    fn refusal(&self, svc: &dyn Service, req: &RtRequest) -> Option<RtResponse> {
        let protocol = svc
            .override_protocol()
            .unwrap_or_else(|| svc.definition().protocol());
        let mut deserializer = protocol.deserialize_input(req, Judged::INPUT_SCHEMA).ok()?;
        let read = <Judged::Input as DeserializableStruct>::deserialize(&mut *deserializer);
        drop(deserializer);
        match read {
            // The document reads, or the one failure that is a VIOLATION rather than an
            // unreadable document: both are left to the layers behind this wrapper.
            Ok(_) | Err(SerdeError::MissingMember { .. }) => None,
            Err(_) => Some(svc.definition().refusal_response(
                // No violation to carry: nothing was built, so there is no member to name
                // and no value to echo. The wording travels with the refusal instead, and
                // this service's renderer answers it in the envelope every other answer of
                // this service carries (`crate::service::EmulatedService::refusal_response`).
                ProtocolError::Invalid(Vec::new()),
                Some(Refused {
                    code: crate::state::MALFORMED_XML_CODE.to_string(),
                    message: crate::state::MALFORMED_XML_MESSAGE.to_string(),
                }),
                req,
            )),
        }
    }
}
