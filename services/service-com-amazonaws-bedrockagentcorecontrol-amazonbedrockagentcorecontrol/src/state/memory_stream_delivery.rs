// SPDX-License-Identifier: CC0-1.0

//! The dependency a memory's `streamDeliveryResources` names: the Kinesis data stream the
//! memory's records are delivered to, and whether this account really holds it.
//!
//! A memory configured for stream delivery writes its records INTO that stream, so the
//! stream is part of what the create persists rather than an opaque string it echoes back:
//! AWS resolves it — and the execution role's access to it — before it accepts the create,
//! and refuses one naming a stream that cannot be reached with the operation's modelled
//! `ValidationException` (HTTP 400), creating no memory. Without that resolution a caller
//! that mistypes an ARN gets a memory that will never deliver anything instead of an
//! immediate, actionable refusal.
//!
//! The stream is resolved IN-PROCESS, out of the Kinesis service's OWN store on the one
//! engine every service of this emulator shares — the store `kinesis:CreateStream` writes
//! and `kinesis:DescribeStreamSummary` answers from — the same way this crate's
//! authorization gate reads the IAM and STS stores. Nothing here opens a socket, nothing
//! leaves this process, and no part of the request reaches a network destination: the ARN
//! the request carries is compared against the ARNs the Kinesis store holds. The refusal is
//! therefore grounded in what that store says, never assumed — an emulator with no Kinesis
//! mounted refuses nothing.

use std::time::SystemTime;

use ::service_com_amazonaws_kinesis_kinesis_20131202::state as kinesis_store;

use super::*;
use crate::ops::types::StreamDeliveryResource;

/// Why a memory whose stream-delivery resource names a stream that cannot be reached is
/// refused, as AWS words it for this error class: the role cannot reach the stream, and the
/// sentence names the two permissions to check when the stream is KMS-encrypted. The whole
/// text is fixed — nothing of the request is echoed, so the remedy reads the same whichever
/// ARN was named.
pub const KINESIS_STREAM_INACCESSIBLE: &str =
    "Memory execution role does not have access to provided Kinesis stream. If the stream is \
     KMS-encrypted, ensure the role has kms:GenerateDataKey and kms:Decrypt permissions on the \
     KMS key.";

/// The modelled validation error for a stream-delivery resource this account cannot
/// deliver to.
///
/// `reason` is [`VALIDATION_REASON_FIELD_VALIDATION_FAILED`] — a member of the request names
/// a resource that does not resolve — and there is no `fieldList`: the sentence states the
/// whole remedy, and the recorded refusal carries no per-member entry.
pub fn stream_delivery_inaccessible() -> ValidationException {
    ValidationException {
        message: KINESIS_STREAM_INACCESSIBLE.to_string(),
        reason: VALIDATION_REASON_FIELD_VALIDATION_FAILED.to_string(),
        field_list: None,
    }
}

/// The ARN vendor (service namespace) a Kinesis data stream's ARN carries:
/// `aws.api#service$arnNamespace` of the Kinesis model.
const KINESIS_VENDOR: &str = "kinesis";

/// Whether a stream one of `resources`' delivery configurations names is one the emulator's
/// Kinesis does not hold.
///
/// `true` refuses the create or the update. `false` means it may be written, which includes
/// every case where nothing is KNOWN about the stream — a configuration naming no stream at
/// all, or an emulator with no Kinesis store to read — because a refusal has to be grounded
/// in what the owning service holds.
///
/// Called with no guard of this service's own store held: it reads another service's store.
pub fn stream_delivery_is_unreachable<Eng: Send + Sync + 'static>(
    engine: &Eng,
    resources: Option<&StreamDeliveryResources>,
) -> bool {
    let Some(resources) = resources else {
        return false;
    };
    let Some(kinesis) = kinesis_store::service_state(engine) else {
        return false;
    };
    let now = SystemTime::now();
    let store = kinesis.lock();
    resources.resources.iter().any(|resource| {
        let StreamDeliveryResource::Kinesis(kinesis) = resource;
        let arn = &kinesis.data_stream_arn;
        // A stream lives behind the endpoint of its OWN Region, which is the Region field of
        // its ARN, read through `service_common`'s one ARN parse — it reports that field only
        // for an ARN OF KINESIS whose Region is a Region label. Anything else is looked for
        // in this emulator's default Region, where it will not be found either.
        let region = ::service_common::arn::region_of(arn, KINESIS_VENDOR).unwrap_or(DEFAULT_REGION);
        store.stream_by_arn(now, region, arn).is_none()
    })
}
