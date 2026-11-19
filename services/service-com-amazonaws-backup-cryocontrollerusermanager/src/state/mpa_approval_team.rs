// SPDX-License-Identifier: CC0-1.0

//! Whether the MULTI-PARTY APPROVAL TEAM a request names exists at all, asked of the
//! service that mints teams.
//!
//! # The rule this module implements
//!
//! A member that names a resource of ANOTHER service is not accepted on its FORM alone:
//! the service that owns the resource is the only component that knows whether it holds
//! it, so the value is resolved against that service BEFORE this one records anything, and
//! a value it answers it holds no such resource under is refused as that member's value —
//! nothing is persisted, so no later read reports a vault governed by a team that does not
//! exist. For `MpaApprovalTeamArn` that is the whole decision a client depends on: a vault
//! reported as subject to multi-party approval has an approval team, or the call that
//! would have put it under one was refused and the caller has an error to catch.
//!
//! The FORM is read first ([`super::rejected_mpa_approval_team_arn`]), because a value
//! that is not a team's ARN at all names no team in any account; both halves answer the
//! SAME refusal ([`super::rejected_parameter`]), so the answer discloses nothing about
//! which teams exist.
//!
//! The question is put to the service that MINTS teams, over the emulator's own in-process
//! transport ([`smithy_server_runtime::EmbeddedEmulator`]) — the same hop a restore
//! re-creates a protected resource through ([`super::perform_restore`]) and a backup asks
//! about one through ([`super::protected_resource_answer`]) — and it is answered from that
//! service's OWN read of its own records, so a team minted through its create resolves on
//! the next call and one it no longer holds stops resolving. It is asked with NO store
//! guard held.
//!
//! Nothing is decided on an answer this emulator could not get: a hop that fails, an
//! emulator with that service not mounted, or an answer carrying no team at all leave the
//! value accepted on its form, the way the protected-resource lookup leaves a job's
//! outcome alone when the owning service cannot be asked. Only that service ANSWERING
//! that the caller has no such team grounds the refusal.
//!
//! This is the same shape of decision this service already takes about the `IamRoleArn` a
//! backup selection would be stored with ([`super::rejected_selection_role`]): resolve the
//! other service's resource before persisting anything, refuse it as the member's value,
//! and accept whatever is not an answer from that service.

use smithy_server_runtime::EmbeddedEmulator;

use crate::ops::types::InvalidParameterValueException;

/// What the service that mints approval teams answered about one team ARN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalTeam {
    /// It reports that it holds the team.
    Held,
    /// It ANSWERED that the caller has no team under that ARN.
    Absent,
    /// Nothing was learned: see the module docs.
    Unknown,
}

/// The modelled refusal for a member that names an MPA approval team whose value is not a
/// team's ARN, or names a team the service that mints teams does not hold, or `None` when
/// it names one.
///
/// The ONE resolution step a call that takes such a member runs before it writes.
pub async fn rejected_approval_team(
    callback: &EmbeddedEmulator,
    member: &str,
    value: &str,
) -> Option<InvalidParameterValueException> {
    if let Some(refusal) = super::rejected_mpa_approval_team_arn(member, value) {
        return Some(refusal);
    }
    match approval_team_answer(callback, value).await {
        ApprovalTeam::Absent => Some(super::rejected_parameter(member)),
        ApprovalTeam::Held | ApprovalTeam::Unknown => None,
    }
}

/// What the service that mints approval teams answers about the team `arn` names.
///
/// Asked in the Region the ARN itself names ([`super::mpa_approval_team_arn_parts`]), which
/// is where the team it names lives; `arn` has passed the form check, so it names one.
async fn approval_team_answer(callback: &EmbeddedEmulator, arn: &str) -> ApprovalTeam {
    let Some((region, account_id)) = super::mpa_approval_team_arn_parts(arn) else {
        return ApprovalTeam::Unknown;
    };
    let answer = approval_team_client(callback, region)
        .get_approval_team()
        .arn(arn)
        .send()
        .await;
    match answer {
        // An answer carrying no team is this emulator failing to READ an answer rather
        // than that service reporting the team absent.
        Ok(team) => {
            if team.arn().is_some() {
                ApprovalTeam::Held
            } else {
                ApprovalTeam::Unknown
            }
        }
        Err(error) => {
            // Two answers say the caller has no such team: the team ARN resolves to
            // nothing, and an ARN of ANOTHER account, which that service refuses to read
            // at all — no team of another account governs this account's vaults. The
            // second is read only for an ARN that does name another account, so a denial
            // about a team of THIS account is a lookup that was not answered rather than
            // a team reported absent.
            let names_another_account = account_id != ::engine::emulator_account_id();
            let absent = error.as_service_error().is_some_and(|service_error| {
                service_error.is_resource_not_found_exception()
                    || (names_another_account && service_error.is_access_denied_exception())
            });
            if absent {
                ApprovalTeam::Absent
            } else {
                ApprovalTeam::Unknown
            }
        }
    }
}

/// The client that question is asked through.
///
/// Configured exactly as this tree's other in-process clients are
/// ([`super::dynamodb_client`], `::service_common::eventbridge_client`): the callback routes
/// IN PROCESS to this emulator's own inbound listener, so the endpoint URL is addressing
/// for that transport rather than a host anything connects to; the credentials are the
/// callback's, which sign this hop and never appear in a response; and there is nothing to
/// retry, time out or watch for a stalled stream over an in-process transport.
///
/// `region` is the Region segment of the ARN the request named, which has already been
/// held to the form a team's ARN takes ([`super::mpa_approval_team_arn_parts`]): lower-case
/// letters, digits and hyphens, at most the length that form admits, so a value a caller
/// chose cannot become anything but a Region in that addressing.
fn approval_team_client(callback: &EmbeddedEmulator, region: &str) -> ::aws_sdk_mpa::Client {
    let config = ::aws_sdk_mpa::Config::builder()
        .behavior_version(::aws_sdk_mpa::config::BehaviorVersion::latest())
        .region(::aws_sdk_mpa::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://mpa.{region}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(::aws_sdk_mpa::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_mpa::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_mpa::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_mpa::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_mpa::Client::from_conf(config)
}
