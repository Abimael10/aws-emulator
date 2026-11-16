// SPDX-License-Identifier: CC0-1.0

//! How this service's `awsJson1_0` responses carry a REFUSAL: the status every client
//! error is answered with, and the body member an authorization refusal states its
//! sentence in.
//!
//! Both are properties of the wire protocol rather than of any one operation, so both
//! live here once: [`PcsJsonProtocol`] is the protocol `service.rs` hands the dispatch
//! sequence through [`smithy_server_runtime::Service::override_protocol`], so every
//! modelled failure of every operation goes out through it and no handler mentions the
//! rendering at all. Everything else — routing, request deserialization and every
//! success body — is the framework's own `awsJson1_0`, delegated to unchanged.
//!
//! ## The status
//!
//! `awsJson1_0` names the error in the body's `__type`, not in the status line, and this
//! service answers ONE status for the whole client-error class: the recorded refusals of
//! this API are HTTP 400 for error shapes whose `@httpError` is 404 (the
//! `ResourceNotFoundException` family) and 403 (`AccessDeniedException`) alike. See
//! [`error_status`].
//!
//! ## The member an authorization refusal's sentence travels in
//!
//! A refusal of the CALLER — `AccessDeniedException` — is answered by the layer in front
//! of the API rather than by the API itself, and that layer's error document carries its
//! sentence in the `Message` member, where this service's own modelled refusals (a
//! not-found, a validation failure, a conflict, a quota) carry theirs in `message`. The
//! error shapes of the model declare the latter spelling, so the former is applied here.
//!
//! The rule is keyed on the error CLASS ([`CALLER_REFUSAL_SHAPES`]), not on which handler
//! produced it: a refusal of the caller is the same answer whichever operation it refuses,
//! and this service's two of them — the cluster-boundary refusal every cluster-scoped
//! operation answers, and `RegisterComputeNodeGroupInstance`'s refusal of a caller that is
//! not one of the compute node instances PCS launched — are both the front layer's rather
//! than the API's, so neither is reported in a different member from the other. An SDK
//! reads whichever of the two spellings a body carries, so a client sees the sentence
//! either way. The renaming itself is `service-common`'s, the one the tree states for
//! every service whose wire member differs from the one a layer wrote; this file states
//! only the two member names and the class the rule covers.

use aws_smithy_runtime_api::http::{Request, Response};
use aws_smithy_schema::serde::{SerializableStruct, ShapeDeserializer};
use aws_smithy_schema::{Schema, ShapeId};
use aws_smithy_schema_server::json::JsonCodec;
use aws_smithy_schema_server::protocol::{ProtocolError, ServerProtocol};
use aws_smithy_schema_server::rpc::HttpRpcServerProtocol;

/// The HTTP status every CLIENT error of this service is answered with.
///
/// Public: this crate's own suites assert refusals against it rather than restating the
/// number, so the status a test expects and the status the wire carries are one value.
pub const CLIENT_ERROR_STATUS: u16 = 400;

/// The error shapes whose sentence this service reports in [`CALLER_MESSAGE_MEMBER`]: the
/// refusal of the CALLER, which the layer in front of the API answers (see the module
/// header).
const CALLER_REFUSAL_SHAPES: [&str; 1] = ["AccessDeniedException"];

/// The body member a refusal of the caller carries its sentence in.
const CALLER_MESSAGE_MEMBER: &str = "Message";

/// The body member every other error of this service carries its sentence in, which is
/// also the member each error shape of the model declares
/// (`smithy_query(query="shape",shape="AccessDeniedException,ResourceNotFoundException,ValidationException")`).
const MODELLED_MESSAGE_MEMBER: &str = "message";

/// The status a modelled error is answered with, from the status its shape declares.
///
/// A client error (`4xx`) is answered with [`CLIENT_ERROR_STATUS`] whatever `@httpError`
/// the shape carries, so a caller reads WHICH refusal it was from the error code in the
/// body — the only place this service reports it. A server error keeps its own status:
/// nothing recorded contradicts it, and a `5xx` is what tells a client the call may be
/// retried as sent.
#[must_use]
pub fn error_status(modelled_status: u16) -> u16 {
    match modelled_status {
        400..=499 => CLIENT_ERROR_STATUS,
        other => other,
    }
}

/// `resp` as this service answers a refusal of the caller: its sentence in the member
/// that layer reports it in (see the module header).
fn with_caller_message_member(resp: Response) -> Response {
    ::service_common::error_detail_member::with_renamed_detail_member(
        resp,
        &CALLER_REFUSAL_SHAPES,
        MODELLED_MESSAGE_MEMBER,
        CALLER_MESSAGE_MEMBER,
    )
}

/// This service's wire protocol: `awsJson1_0` exactly as the model declares it, with the
/// refusal envelope this service answers — one status for the client-error class, and an
/// authorization refusal's sentence in the member that layer reports it in.
#[derive(Debug)]
pub struct PcsJsonProtocol {
    inner: HttpRpcServerProtocol<JsonCodec>,
}

impl PcsJsonProtocol {
    /// Build the protocol once, at service construction: `override_protocol` hands back a
    /// borrow of it and must never build one per request.
    ///
    /// The route table the generated entry carries is the model's and is untouched: a
    /// protocol's routes are read by [`ServerProtocol::routing_key`] alone, and an
    /// author-overridden protocol drives only the modelled sequence —
    /// `deserialize_input`, `serialize_output`, `serialize_error`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: aws_smithy_schema_server::AwsJson10(),
        }
    }
}

impl Default for PcsJsonProtocol {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerProtocol for PcsJsonProtocol {
    type Request = Request;
    type Response = Response;

    fn protocol_id(&self) -> &ShapeId {
        self.inner.protocol_id()
    }

    fn routing_key<'a>(&self, request: &'a Self::Request) -> Result<&'a str, ProtocolError> {
        self.inner.routing_key(request)
    }

    fn deserialize_input<'a>(
        &self,
        request: &'a Self::Request,
        input_schema: &Schema,
    ) -> Result<Box<dyn ShapeDeserializer + 'a>, ProtocolError> {
        self.inner.deserialize_input(request, input_schema)
    }

    fn serialize_output(
        &self,
        output: &dyn SerializableStruct,
        output_schema: &Schema,
        operation_name: &str,
        success_status: u16,
    ) -> Result<Self::Response, ProtocolError> {
        self.inner
            .serialize_output(output, output_schema, operation_name, success_status)
    }

    fn serialize_error(
        &self,
        error: &dyn SerializableStruct,
        error_schema: &Schema,
        error_code: &str,
        http_status: u16,
    ) -> Result<Self::Response, ProtocolError> {
        // The envelope is the framework's own `awsJson1_0`; what this service decides is
        // the STATUS — one value for the whole client-error class rather than the one each
        // error shape's `@httpError` declares — and the member a refusal of the CALLER
        // states its sentence in.
        let rendered = self.inner.serialize_error(
            error,
            error_schema,
            error_code,
            error_status(http_status),
        )?;
        Ok(with_caller_message_member(rendered))
    }

    fn serialize_event_stream_message(
        &self,
        event_type: &str,
        is_exception: bool,
        event: &dyn SerializableStruct,
        event_schema: &Schema,
    ) -> Result<Self::Response, ProtocolError> {
        self.inner
            .serialize_event_stream_message(event_type, is_exception, event, event_schema)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: every client error is answered with one status, and a server error keeps its
    /// own. The rows span the `@httpError` values this service's error shapes declare —
    /// `smithy_query(query="operation",operation="GetCluster")` lists them: 400
    /// `ValidationException`, 402 `ServiceQuotaExceededException`, 403
    /// `AccessDeniedException`, 404 `ResourceNotFoundException`, 409
    /// `ConflictException`, 429 `ThrottlingException`, 500
    /// `InternalServerException` — plus the scaffold channel's own 501.
    #[test]
    fn every_client_error_is_answered_with_one_status() {
        for modelled in [400_u16, 402, 403, 404, 409, 429] {
            assert_eq!(
                error_status(modelled),
                CLIENT_ERROR_STATUS,
                "case a modelled {modelled}: expected the client-error status, got {}",
                error_status(modelled)
            );
        }
        for modelled in [500_u16, 501, 503] {
            assert_eq!(
                error_status(modelled),
                modelled,
                "case a modelled {modelled}: expected a server status to be kept, got {}",
                error_status(modelled)
            );
        }
    }
}
