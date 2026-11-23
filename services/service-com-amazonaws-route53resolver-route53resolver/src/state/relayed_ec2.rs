// SPDX-License-Identifier: CC0-1.0

//! How this service reports a refusal it relayed from the account's Amazon EC2
//! inventory.
//!
//! Several of this service's calls resolve something EC2 owns — the VPC collection
//! a per-VPC listing pages, and the VPCs a write names — by CALLING that service,
//! so a refusal one of those calls comes back with belongs to EC2 rather than to
//! Route 53 Resolver. Resolver does not re-word it: the recorded refusals of those
//! paths carry EC2's own sentence inside the clause the SDK layer that relayed the
//! call states around a caught service exception — who refused, with what HTTP
//! status, under which error code, and the id of that call — and then Resolver's
//! own trace clause for the request it was answering.
//!
//! The envelope itself is rendered through the ONE home for that rendering in this
//! tree ([`::service_common::relayed_refusal`]), so a client parses one form
//! whichever emulated service relayed a call, and the request id in it belongs to
//! the RELAYED CALL, so one is minted per call rather than derived from anything
//! the caller sent.

use super::*;

/// The service these refusals come from, as the SDK layer that relayed the call
/// names it — neither EC2's endpoint prefix nor the casing of its `sdkId`.
const EC2_SERVICE_NAME: &str = "AmazonEC2";

/// The HTTP status Amazon EC2 answers a refusal of the caller's own input with.
const EC2_REFUSAL_STATUS: u16 = 400;

/// The Amazon EC2 error code for a value that service will not read as the
/// parameter it was handed as — what a pagination cursor its own listings never
/// issued is refused under.
pub const EC2_CODE_INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// `sentence`, as Amazon EC2 worded it, reported the way this service reports a
/// refusal it relayed from that inventory: the sentence, the caught-exception
/// clause naming the call it came from under `code`, and the trace clause of the
/// request this service was answering.
pub fn relayed_ec2_message(sentence: &str, code: &str, trace_root: &str) -> String {
    let relayed = ::service_common::relayed_refusal::caught_exception_message(
        "",
        "",
        sentence,
        ::service_common::relayed_refusal::CaughtCall {
            service: EC2_SERVICE_NAME,
            status: EC2_REFUSAL_STATUS,
            code,
            request_id: &::service_common::query_error_envelope::new_request_id(),
        },
    );
    traced_message(relayed, trace_root)
}
