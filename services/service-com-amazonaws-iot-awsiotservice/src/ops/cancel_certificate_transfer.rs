// SPDX-License-Identifier: CC0-1.0

//! @op CancelCertificateTransfer
//!
//! Takes back a pending transfer of one device certificate, the reverse of the write
//! `TransferCertificate` makes.
//!
//! "After a certificate transfer is cancelled, the status of the certificate changes from
//! PENDING_TRANSFER to INACTIVE", and the transfer the certificate reported while it was
//! pending is gone — which is what the next `DescribeCertificate` reads. The write, and the
//! refusal for a certificate that is in no pending transfer, live beside the certificate
//! record in [`crate::state::certificate_transfers`], so one rule serves this operation and
//! the transfer it cancels.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `CancelCertificateTransfer`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json")
//! ```
//!
//! Cancels a pending transfer for the specified certificate. Note Only the transfer source
//! account can use this operation to cancel a transfer. (Transfer destinations can use
//! RejectCertificateTransfer instead.) After transfer, IoT returns the certificate to the
//! source account in the INACTIVE state. After the destination account has accepted the
//! transfer, the transfer cannot be cancelled. After a certificate transfer is cancelled,
//! the status of the certificate changes from PENDING_TRANSFER to INACTIVE. Requires
//! permission to access the CancelCertificateTransfer action.
//!
//! ## Input
//!
//! * `certificateId` (CertificateId, len 64..64, pattern ^(0x)?[a-fA-F0-9]+$) **required** —
//!   The ID of the certificate. (The last part of the certificate ARN contains the
//!   certificate ID.)
//!
//! ## Errors
//!
//! * `InternalFailureException` (HTTP 500) — An unexpected error has occurred.
//! * `InvalidRequestException` (HTTP 400) — The request is not valid.
//! * `ResourceNotFoundException` (HTTP 404) — The specified resource does not exist.
//! * `ServiceUnavailableException` (HTTP 503) — The service is temporarily unavailable.
//! * `ThrottlingException` (HTTP 400) — The rate exceeds the limit.
//! * `TransferAlreadyCompletedException` (HTTP 410) — You can't revert the certificate
//!   transfer because the transfer is already complete.
//! * `UnauthorizedException` (HTTP 401) — You are not authorized to perform this operation.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `CancelCertificateTransferError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-iot-awsiotservice.json");

#[derive(Debug, Clone, Default)]
pub struct CancelCertificateTransferOutput;

impl CancelCertificateTransferOutput {
    // `with_http` carries this operation's modelled response status. Remove it and
    // the status falls back to the protocol default, so a modelled 204 answers 200.
    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static("smithy.api#Unit", "smithy.api", "Unit"),
        ::aws_smithy_schema::ShapeType::Structure,
        &[],
    )
    .with_http(::aws_smithy_schema::traits::HttpTrait::new(
        "PATCH",
        "/cancel-certificate-transfer/{certificateId}",
        Some(200u16),
    ));
}

pub static CANCEL_CERTIFICATE_TRANSFER_OUTPUT_SCHEMA: &::aws_smithy_schema::Schema = &CancelCertificateTransferOutput::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for CancelCertificateTransferOutput {
    fn serialize_members(&self, _s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer) -> Result<(), ::aws_smithy_schema::serde::SerdeError> { Ok(()) }
}

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `CancelCertificateTransferError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.iot#CancelCertificateTransfer")]
#[derive(Debug)]
pub enum CancelCertificateTransferErrorModeled {
    InternalFailureException(InternalFailureException),
    InvalidRequestException(InvalidRequestException),
    ResourceNotFoundException(ResourceNotFoundException),
    ServiceUnavailableException(ServiceUnavailableException),
    ThrottlingException(ThrottlingException),
    TransferAlreadyCompletedException(TransferAlreadyCompletedException),
    UnauthorizedException(UnauthorizedException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::cancel_certificate_transfer::CancelCertificateTransferError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type CancelCertificateTransferError = crate::scaffolded::Scaffolded<CancelCertificateTransferErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<CancelCertificateTransferErrorModeled> for CancelCertificateTransferError {
    fn from(modeled: CancelCertificateTransferErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// Record this state-changing call for attribution: the operation, the identity the
/// request PRESENTED (the SigV4 access key id, a public identifier and never a secret),
/// the request target, the certificate the call named and how the call ended.
///
/// A cancellation ends a transfer of a device identity to another account, so WHO took it
/// back and when is not recoverable from the record afterwards — the cancelled transfer
/// leaves nothing behind to be read. Every request-derived field goes through
/// [`crate::state::audit_field`], which bounds it and strips the control characters that
/// would otherwise forge a second record; the subscriber supplies the timestamp.
fn audit(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    certificate_id: &str,
    outcome: &'static str,
) {
    let parts = ctx.parts();
    ::tracing::info!(
        target: "emulator::audit",
        service = ctx.identity().signing_name(),
        operation = "CancelCertificateTransfer",
        access_key_id = %crate::state::audit_field(&::engine::extract_akid(&parts.headers)),
        request_target =
            %crate::state::audit_field(&format!("{} {}", parts.method, parts.uri.path())),
        certificate_id = %crate::state::audit_field(certificate_id),
        outcome,
        "state-changing operation"
    );
}

/// The `CancelCertificateTransfer` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct CancelCertificateTransfer<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> CancelCertificateTransfer<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for CancelCertificateTransfer<Eng>
{
    type Input = CancelCertificateTransferInput;
    type Output = CancelCertificateTransferOutput;
    type Error = CancelCertificateTransferError;

    const NAME: &'static str = "CancelCertificateTransfer";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::CANCEL_CERTIFICATE_TRANSFER_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = CANCEL_CERTIFICATE_TRANSFER_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            CancelCertificateTransferErrorModeled::InternalFailureException(
                InternalFailureException {
                    message: Some("the IoT emulator has no state store attached".to_string()),
                },
            )
        })?;

        // Whether there is a transfer to cancel is the certificate's own state, so the
        // read and the write happen under one lock: two cancellations of one transfer
        // cannot both find it pending.
        let cancelled = state
            .lock()
            .cancel_certificate_transfer(&input.certificate_id, ::std::time::SystemTime::now());

        // What the call DID: the cancelled transfer is gone from the record, so the status
        // it left is all a later reader can recover.
        audit(
            ctx,
            &input.certificate_id,
            match &cancelled {
                Ok(_) => "transfer-cancelled",
                Err(crate::state::CertificateTransferCancellationRefusal::NotFound(_)) => {
                    "no-such-certificate"
                }
                Err(crate::state::CertificateTransferCancellationRefusal::NotInTransfer(_)) => {
                    "no-transfer-in-progress"
                }
            },
        );

        // Each refusal is reported as the error class the model declares for it, with the
        // one sentence the state words it in, rather than as a hollow success. An id with
        // no pending transfer under it names nothing to cancel, whichever of the two
        // reasons it is, so both are the same 404.
        if let Err(refusal) = cancelled {
            let message = Some(refusal.message().to_string());
            return Err(match refusal {
                crate::state::CertificateTransferCancellationRefusal::NotFound(_)
                | crate::state::CertificateTransferCancellationRefusal::NotInTransfer(_) => {
                    CancelCertificateTransferErrorModeled::ResourceNotFoundException(
                        ResourceNotFoundException { message },
                    )
                }
            }
            .into());
        }

        // The operation's output shape is `Unit`: a successful cancellation reports
        // nothing, and the status it left is read back through `DescribeCertificate`.
        Ok(CancelCertificateTransferOutput)
    }
}
