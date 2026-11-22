// SPDX-License-Identifier: CC0-1.0

//! @op TransferCertificate
//!
//! Starts the transfer of one device certificate to another Amazon Web Services account.
//!
//! The transfer is a STATE the certificate enters rather than an event: "You can cancel the
//! transfer until it is accepted by the recipient", so the certificate stays in this
//! account's store carrying `PENDING_TRANSFER` and the transfer the request named, which is
//! what the next `DescribeCertificate` reports as its `transferData`. The write, and the two
//! documented preconditions it is refused for — a certificate that "must not be in the
//! ACTIVE state" and one that "must not have any policies attached to it" — live beside the
//! certificate record in [`crate::state::certificate_transfers`], so one rule serves this
//! operation and the cancellation that takes the transfer back.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `TransferCertificate`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json")
//! ```
//!
//! Transfers the specified certificate to the specified Amazon Web Services account.
//! Requires permission to access the TransferCertificate action. You can cancel the
//! transfer until it is accepted by the recipient. No notification is sent to the transfer
//! destination's account. The caller is responsible for notifying the transfer target. The
//! certificate being transferred must not be in the ACTIVE state. You can use the
//! UpdateCertificate action to deactivate it. The certificate must not have any policies
//! attached to it. You can use the DetachPolicy action to detach them. Customer managed key
//! behavior: When you use a customer managed key to encrypt your data and then transfer the
//! certificate to a customer in a different account using the TransferCertificate
//! operation, the certificates will no longer be encrypted by their customer managed key
//! configuration. During the transfer process, certificates are encrypted using Amazon Web
//! Services IoT Core owned keys. While a certificate is in the PENDING_TRANSFER state, it's
//! always protected by Amazon Web Services IoT Core owned keys, regardless of the customer
//! managed key configuration of either the source or destination account. Once the transfer
//! is completed through AcceptCertificateTransfer, RejectCertificateTransfer, or
//! CancelCertificateTransfer, the certificate will be protected by the customer managed key
//! configuration of the account that owns the certificate after the transfer operation: If
//! the transfer is accepted: The certificate is encrypted by the target account's customer
//! managed key configuration. If the transfer is rejected or cancelled: The certificate is
//! protected by the source account's customer managed key configuration.
//!
//! ## Input
//!
//! * `certificateId` (CertificateId, len 64..64, pattern ^(0x)?[a-fA-F0-9]+$) **required** —
//!   The ID of the certificate. (The last part of the certificate ARN contains the
//!   certificate ID.)
//! * `targetAwsAccount` (AwsAccountId, len 12..12, pattern ^[0-9]+$) **required** — The
//!   Amazon Web Services account.
//! * `transferMessage` (Message, len 0..128, pattern ^[\s\S]*$) — The transfer message.
//!
//! ## Output
//!
//! * `transferredCertificateArn` (CertificateArn) — The ARN of the certificate.
//!
//! ## Errors
//!
//! * `CertificateStateException` (HTTP 406) — The certificate operation is not allowed.
//! * `InternalFailureException` (HTTP 500) — An unexpected error has occurred.
//! * `InvalidRequestException` (HTTP 400) — The request is not valid.
//! * `ResourceNotFoundException` (HTTP 404) — The specified resource does not exist.
//! * `ServiceUnavailableException` (HTTP 503) — The service is temporarily unavailable.
//! * `ThrottlingException` (HTTP 400) — The rate exceeds the limit.
//! * `TransferConflictException` (HTTP 409) — You can't transfer the certificate because
//!   authorization policies are still attached.
//! * `UnauthorizedException` (HTTP 401) — You are not authorized to perform this operation.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `TransferCertificateError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-iot-awsiotservice.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `TransferCertificateError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.iot#TransferCertificate")]
#[derive(Debug)]
pub enum TransferCertificateErrorModeled {
    CertificateStateException(CertificateStateException),
    InternalFailureException(InternalFailureException),
    InvalidRequestException(InvalidRequestException),
    ResourceNotFoundException(ResourceNotFoundException),
    ServiceUnavailableException(ServiceUnavailableException),
    ThrottlingException(ThrottlingException),
    TransferConflictException(TransferConflictException),
    UnauthorizedException(UnauthorizedException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::transfer_certificate::TransferCertificateError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type TransferCertificateError = crate::scaffolded::Scaffolded<TransferCertificateErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<TransferCertificateErrorModeled> for TransferCertificateError {
    fn from(modeled: TransferCertificateErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// Record this state-changing call for attribution: the operation, the identity the
/// request PRESENTED (the SigV4 access key id, a public identifier and never a secret),
/// the request target, the certificate and the account the call named and how the call
/// ended.
///
/// A transfer hands a device identity to another account, so WHO started it, for which
/// certificate and to which account is not recoverable from the record afterwards — a
/// cancelled or completed transfer leaves no transfer behind to read. Every
/// request-derived field goes through [`crate::state::audit_field`], which bounds it and
/// strips the control characters that would otherwise forge a second record; the
/// subscriber supplies the timestamp.
fn audit(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    certificate_id: &str,
    target_aws_account: &str,
    outcome: &'static str,
) {
    let parts = ctx.parts();
    ::tracing::info!(
        target: "emulator::audit",
        service = ctx.identity().signing_name(),
        operation = "TransferCertificate",
        access_key_id = %crate::state::audit_field(&::engine::extract_akid(&parts.headers)),
        request_target =
            %crate::state::audit_field(&format!("{} {}", parts.method, parts.uri.path())),
        certificate_id = %crate::state::audit_field(certificate_id),
        target_aws_account = %crate::state::audit_field(target_aws_account),
        outcome,
        "state-changing operation"
    );
}

/// The `TransferCertificate` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct TransferCertificate<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> TransferCertificate<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for TransferCertificate<Eng>
{
    type Input = TransferCertificateInput;
    type Output = TransferCertificateOutput;
    type Error = TransferCertificateError;

    const NAME: &'static str = "TransferCertificate";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::TRANSFER_CERTIFICATE_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::TRANSFER_CERTIFICATE_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            TransferCertificateErrorModeled::InternalFailureException(InternalFailureException {
                message: Some("the IoT emulator has no state store attached".to_string()),
            })
        })?;

        // What may hold the transfer back is the account's own state — the certificate's
        // status and the policies attached to it — so the whole decision and the whole
        // write happen under one lock: no caller can observe the certificate between the
        // state the call read and the transfer it left.
        let transferred = state.lock().transfer_certificate(
            &input.certificate_id,
            &input.target_aws_account,
            input.transfer_message.as_deref(),
            ::std::time::SystemTime::now(),
        );

        // What the call DID: a transfer that is later cancelled, accepted or rejected
        // leaves no transfer behind for a reader to recover it from.
        audit(
            ctx,
            &input.certificate_id,
            &input.target_aws_account,
            match &transferred {
                Ok(_) => "transfer-started",
                Err(crate::state::CertificateTransferRefusal::NotFound(_)) => {
                    "no-such-certificate"
                }
                Err(crate::state::CertificateTransferRefusal::State(_)) => "certificate-state",
                Err(crate::state::CertificateTransferRefusal::AttachedPolicies(_)) => {
                    "policies-attached"
                }
            },
        );

        // Each refusal is reported as the error class the model declares for it, with the
        // one sentence the state words it in, rather than as a hollow success.
        let transferred = match transferred {
            Ok(certificate) => certificate,
            Err(refusal) => {
                let message = Some(refusal.message().to_string());
                return Err(match refusal {
                    crate::state::CertificateTransferRefusal::NotFound(_) => {
                        TransferCertificateErrorModeled::ResourceNotFoundException(
                            ResourceNotFoundException { message },
                        )
                    }
                    crate::state::CertificateTransferRefusal::State(_) => {
                        TransferCertificateErrorModeled::CertificateStateException(
                            CertificateStateException { message },
                        )
                    }
                    crate::state::CertificateTransferRefusal::AttachedPolicies(_) => {
                        TransferCertificateErrorModeled::TransferConflictException(
                            TransferConflictException { message },
                        )
                    }
                }
                .into());
            }
        };

        // "The ARN of the certificate" (`transferredCertificateArn`): the ARN of the
        // record the transfer was written on, so what this call reports and what the next
        // `DescribeCertificate` reads are one value and not two that could drift.
        Ok(TransferCertificateOutput {
            transferred_certificate_arn: Some(transferred.certificate_arn),
        })
    }
}
