// SPDX-License-Identifier: CC0-1.0

//! Whether the Amazon EFS file system a transfer location names is one Amazon EFS HOLDS.
//!
//! `CreateLocationEfs$EfsFilesystemArn` is "the ARN for your Amazon EFS file system", and the
//! member's `@length`/`@pattern` can say only that a value is SHAPED like a file system ARN —
//! never that the account has that file system. A file system is a resource of Amazon EFS, so
//! that service is the one answer to "does this account hold it", and the location create
//! resolves the reference there before it stores anything: a location pointing at a file
//! system nobody created could never be read or written by a transfer, and a client that gets
//! a `LocationArn` for it discovers the misconfiguration from a task execution instead of
//! from the call that made the mistake.
//!
//! Amazon EFS is reached the way any emulated service reaches another — the AWS SDK driven
//! over the in-process callback ([`smithy_server_runtime::OperationContext::callback`]) — so
//! this crate names no other service's crate, no socket is opened and nothing outside this
//! process is reachable. The question is put with `DescribeFileSystems`, whose `FileSystemId`
//! member takes the file system's id OR its ARN and whose modelled `FileSystemNotFound`
//! (HTTP 404) is "returned if the specified FileSystemId value doesn't exist in the
//! requester's Amazon Web Services account" — exactly the fact this reading needs.
//!
//! This is a GATE: the location keeps the ARN the request carried and nothing of the file
//! system's own record, so a refusal is served only where Amazon EFS ANSWERED that the file
//! system is not there (`services/common/src/network.rs` states which cross-service reading
//! refuses on silence and which does not). Everything else establishes nothing and refuses
//! nothing: a value naming a file system of another account, which is not this emulator's to
//! hold, an answer about Amazon EFS rather than about the file system (a fault, a throttle),
//! an answer this emulator could not read, and an emulator with no Amazon EFS mounted to ask.
//!
//! Nothing is cached: a file system created a moment ago resolves at once, and one that was
//! deleted stops resolving at once.

use ::smithy_server_runtime::EmbeddedEmulator;

use crate::ops::types::InvalidRequestException;

/// The ARN namespace of the service that owns file systems, as every `EfsFilesystemArn`
/// pattern in this model spells it.
const EFS_ARN_NAMESPACE: &str = "elasticfilesystem";

/// The class discriminator DataSync reports for a request whose Amazon EFS file system
/// reference it will not accept.
///
/// Every client failure of every operation of this service is the one modelled client error,
/// so the refusal CLASS is carried by that error's two code members rather than by a distinct
/// exception shape — and a caller branching on "my file system reference is wrong" reads it
/// there.
pub const EFS_FILESYSTEM_INVALID_CODE: &str = "EfsFilesystemInvalid";

/// Why an Amazon EFS file system reference was not accepted: the account holds no such file
/// system.
const DOES_NOT_EXIST: &str = "EFS filesystem doesn't exist";

/// How DataSync words a refusal of an Amazon EFS file system reference: fixed text around two
/// slots — the ARN the request itself carried, and the reason that reference was not accepted.
pub fn invalid_file_system_message(filesystem_arn: &str, reason: &str) -> String {
    format!("Invalid EFS filesystem, ARN: {filesystem_arn}, reason: {reason}")
}

/// The modelled client error a request naming a file system Amazon EFS does not hold earns.
///
/// One discriminator fills both code members, like every other refusal class of this service.
pub fn absent_file_system_refusal(filesystem_arn: &str) -> InvalidRequestException {
    let code = Some(EFS_FILESYSTEM_INVALID_CODE.to_string());
    InvalidRequestException {
        message: Some(invalid_file_system_message(filesystem_arn, DOES_NOT_EXIST)),
        error_code: code.clone(),
        datasync_error_code: code,
    }
}

/// Whether Amazon EFS ANSWERED that this account holds no file system the ARN
/// `filesystem_arn` names.
///
/// `request_region` is the Region the call was signed for, used only when the ARN carries no
/// Region label of its own: a file system named by an ARN is resolved in the Region THAT ARN
/// names, because that is the endpoint holding it.
///
/// `false` — establish nothing, refuse nothing — for every answer that is NOT Amazon EFS
/// stating the file system is not there, for the reasons the module documentation gives.
///
/// Call it with NO state guard held — it awaits.
pub async fn file_system_is_absent(
    callback: &EmbeddedEmulator,
    request_region: &str,
    filesystem_arn: &str,
) -> bool {
    let Some(client) = endpoint_holding(callback, request_region, filesystem_arn) else {
        return false;
    };
    let error = match client
        .describe_file_systems()
        .file_system_id(filesystem_arn)
        .send()
        .await
    {
        Ok(_) => return false,
        Err(error) => error,
    };
    // `FileSystemNotFound` is the ONE answer that says this account holds no file system the
    // ARN names; a 5xx is about Amazon EFS rather than about the file system, whatever code
    // travels with it.
    let about_the_file_system = error.as_service_error().is_some_and(
        ::aws_sdk_efs::operation::describe_file_systems::DescribeFileSystemsError::is_file_system_not_found,
    );
    let client_error = error
        .raw_response()
        .is_some_and(|response| response.status().is_client_error());
    if about_the_file_system && client_error {
        return true;
    }
    // Visible to an operator: an answer nothing can be read from establishes nothing, so
    // without this line it would be indistinguishable from a file system Amazon EFS resolved.
    // The caller's own value is rendered in its escaped `Debug` form, so an ARN carrying a
    // newline cannot forge a log line.
    ::tracing::warn!(
        efs_filesystem = ?filesystem_arn,
        error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
        "Amazon EFS gave no answer about this file system, so nothing is established about \
         its existence"
    );
    false
}

/// The Amazon EFS endpoint the file system `filesystem_arn` names is held by, as a client to
/// ask about it — and `None` where the value names no file system of this emulator to ask
/// about at all: text that is not a file system ARN, a file system of another account, which
/// is not this emulator's to hold, and a Region that would name an authority the caller chose.
fn endpoint_holding(
    callback: &EmbeddedEmulator,
    request_region: &str,
    filesystem_arn: &str,
) -> Option<::aws_sdk_efs::Client> {
    let arn = ::service_common::arn::parse_of(filesystem_arn, EFS_ARN_NAMESPACE)?;
    if !arn.account.is_empty() && arn.account != ::engine::emulator_account_id() {
        return None;
    }
    let region = ::service_common::arn::region_label(arn.region)
        .or_else(|| ::service_common::arn::region_label(request_region))?;
    Some(efs_client(callback, region))
}

/// The Amazon EFS client every question about a caller-named file system is put through, for
/// the Region that file system lives in.
///
/// The endpoint URL names Amazon EFS's public host so the request is signed and routed as
/// that service's, but the TRANSPORT is the emulator's own in-process connector. Being
/// in-process, the client disables what a network client needs and this one cannot have:
/// retries, timeouts, stalled-stream protection and identity caching each need a sleep
/// implementation the callback transport is not given. The credentials the callback hands
/// over are the emulator's own: the caller's `Authorization` header is never forwarded.
fn efs_client(callback: &EmbeddedEmulator, region: &str) -> ::aws_sdk_efs::Client {
    let config = ::aws_sdk_efs::Config::builder()
        .behavior_version(::aws_sdk_efs::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_efs::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://elasticfilesystem.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_efs::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_efs::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_efs::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_efs::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_efs::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the refusal names the file system the request asked for and the reason it was not
    /// accepted, and carries the one class discriminator in BOTH code members — whatever the
    /// ARN it echoes looks like.
    #[test]
    fn an_absent_file_system_is_refused_in_one_sentence() {
        // The spellings an `EfsFilesystemArn` admits (the member's own `@pattern`): the
        // partitions it names, the 8-to-40 hexadecimal id widths, and Region labels other
        // than one.
        let arns = [
            "arn:aws:elasticfilesystem:us-west-2:000000000000:file-system/fs-0b52a0f5",
            "arn:aws:elasticfilesystem:eu-west-1:000000000000:file-system/fs-0b52a0f59862d10c7",
            "arn:aws-cn:elasticfilesystem:cn-north-1:000000000000:file-system/fs-abcdef0123456789abcdef0123456789abcdef01",
            "arn:aws-us-gov:elasticfilesystem:us-gov-west-1:000000000000:file-system/fs-deadbeef",
        ];
        for arn in arns {
            let refusal = absent_file_system_refusal(arn);
            let expected =
                format!("Invalid EFS filesystem, ARN: {arn}, reason: EFS filesystem doesn't exist");
            assert_eq!(
                refusal.message.as_deref(),
                Some(expected.as_str()),
                "case {arn:?}: expected the sentence {expected:?}, got {:?}",
                refusal.message
            );
            assert_eq!(
                (
                    refusal.error_code.as_deref(),
                    refusal.datasync_error_code.as_deref()
                ),
                (
                    Some(EFS_FILESYSTEM_INVALID_CODE),
                    Some(EFS_FILESYSTEM_INVALID_CODE)
                ),
                "case {arn:?}: expected both code members to carry the class discriminator, \
                 got {:?} and {:?}",
                refusal.error_code,
                refusal.datasync_error_code
            );
        }
    }
}
