// SPDX-License-Identifier: CC0-1.0

//! Whether this service can ASSUME the service execution role a request names.
//!
//! An application runs AS the role its create hands it — "the IAM role used by the
//! application to access Kinesis data streams, Kinesis Data Firehose delivery streams,
//! Amazon S3 objects, and other external resources"
//! (`smithy_query(query="doc",shape="CreateApplicationRequest",member="ServiceExecutionRole")`)
//! — so this service has to be able to take that role on before an application is stored:
//! one whose role it cannot assume could never read a record or write one, so the request is
//! refused with the modelled `InvalidArgumentException` (HTTP 400) and nothing is written.
//!
//! The question is the tree's shared reading of a caller-supplied execution role
//! ([`::service_common::iam_role::role_is_unassumable`]) — which role of this account the
//! ARN names, the in-process `GetRole`, and the trust-policy reading — so this module states
//! only WHICH principal has to be admitted and HOW this service words the refusal. A refusal
//! is therefore always grounded in what IAM ANSWERED: this account's IAM holds no role under
//! the name the ARN carries, or the trust policy of the role it does hold does not admit this
//! principal. Nothing that is not such an answer refuses anything — a value naming no role of
//! this account, an answer this emulator could not read, an emulator with no IAM mounted at
//! all.
//!
//! THE HOP IS IN-PROCESS AND REACHES NO NETWORK. `service_common::iam_client` drives the SDK
//! over the emulator's own transport through the operation callback
//! ([`smithy_server_runtime::OperationContext::callback`]) — the same seam the Glue Data
//! Catalog read beside it uses — so the `GetRole` is dispatched to the IAM service mounted in
//! THIS process: no socket is opened, no host name is resolved, and no endpoint is built from
//! anything the request carried. `region` names only the credential scope that in-process
//! call is signed under, with the emulator's own credentials the callback supplies; it selects
//! no host, and no part of the request leaves the process.

/// The service principal a service execution role has to trust: this service is what assumes
/// it, in the global spelling a trust policy names a service by.
pub const SERVICE_PRINCIPAL: &str = "kinesisanalytics.amazonaws.com";

/// How this service words a service execution role it cannot assume: the role the request
/// named, echoed, and the fixed sentence that follows it.
pub fn unassumable_service_execution_role_message(role_arn: &str) -> String {
    format!(
        "Kinesis Analytics service doesn't have sufficient privileges to assume the role: \
         {role_arn}. Please check the role provided."
    )
}

/// `true` — refuse the request — when IAM ANSWERED that `role_arn` is a role this service
/// cannot assume, and `false` when it is assumable or when nothing could be read to decide
/// it (see the module docs).
///
/// Awaited with NO guard of this service's store held. The `GetRole` it makes is dispatched
/// IN-PROCESS to the mounted IAM service through the operation callback, so this opens no
/// connection and reaches no host: `region` is the credential scope that in-process call is
/// signed under and never an endpoint.
pub async fn service_execution_role_is_unassumable(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arn: &str,
) -> bool {
    ::service_common::iam_role::role_is_unassumable(ctx, region, role_arn, SERVICE_PRINCIPAL).await
}
