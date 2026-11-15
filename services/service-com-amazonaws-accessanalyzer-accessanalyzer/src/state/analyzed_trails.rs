// SPDX-License-Identifier: CC0-1.0

//! Resolving a trail a policy generation would analyze, against the service that HOLDS it.
//!
//! RULE: `StartPolicyGeneration` analyzes the CloudTrail activity of the trails its
//! `cloudTrailDetails.trails` names ("a `Trail` object that contains settings for a trail",
//! whose `cloudTrailArn` "specifies the ARN of the trail" —
//! `smithy_query(query="shape",shape="CloudTrailDetails,Trail")`), so each of those ARNs
//! names a resource of ANOTHER service. A start therefore RESOLVES every one of them where
//! CloudTrail holds it, synchronously, before any job exists: a trail that service does not
//! hold has no events to analyze, so the request is refused rather than answered with a
//! `jobId` for a generation that could never read anything.
//!
//! CloudTrail is reached the way the framework documents a handler reaching another
//! emulated service ([`smithy_server_runtime::OperationContext::callback`]): an AWS SDK
//! client over the in-process callback's HTTP client and credentials, so nothing here opens
//! a socket or leaves the host. The question is put with `GetTrail`, the operation
//! CloudTrail documents for reading one trail ("returns settings information for a
//! specified trail"), which takes either a trail name or its ARN.
//!
//! The one caller-supplied value that reaches the client's AUTHORITY is the signing Region,
//! and it is narrowed before it gets there ([`::service_common::arn::region_label`]), so no
//! request can re-point this client at a host of its own choosing: it reaches the
//! emulator's own CloudTrail and nothing else, whatever trail a caller names. Which Region
//! the client addresses decides nothing about the answer — a trail ARN carries the Region
//! the trail is homed in, and CloudTrail resolves the ARN by it.
//!
//! WHAT AN UNANSWERED QUESTION MEANS. An emulator assembled with no CloudTrail to ask, or
//! any other failure to reach it, establishes NOTHING about the trail, so it grounds no
//! refusal: the start is served as it was before this resolution existed. Only CloudTrail's
//! own answer that it holds no such trail refuses the request.

use crate::ops::types::ValidationException;

/// How many trail ARNs one start RESOLVES against CloudTrail.
///
/// `TrailList` carries no `@length`, so how many trails a request names is the caller's
/// choice, and each DISTINCT one costs a question put to CloudTrail. The fan-out is
/// therefore bounded here: a request naming more is refused BEFORE the first question
/// rather than resolved in part, so no trail is left unresolved in a start that was served.
/// The ceiling is generous beside the trails an account really has — a trail name is scoped
/// to its Region, and a policy generation analyzes the few trails that carry the principal's
/// activity.
pub const MAX_RESOLVED_TRAILS: usize = 100;

/// The member a refusal about the trails a start names blames.
const TRAILS_MEMBER: &str = "cloudTrailDetails.trails";

/// The ARN resource-type segment of a trail
/// (`arn:aws:cloudtrail:<region>:<account>:trail/<name>`), which is what
/// `CloudTrailArn`'s modelled pattern requires of the value
/// (`^arn:[^:]*:cloudtrail:[^:]*:[^:]*:trail/.{1,576}$`).
const TRAIL_RESOURCE_PREFIX: &str = "trail/";

/// How many `:`-separated fields an ARN has before its resource field, which is therefore
/// everything after them: `arn:<partition>:<service>:<region>:<account>:<resource>`.
const ARN_FIELDS_BEFORE_RESOURCE: usize = 5;

/// What CloudTrail answers about a trail a generation would analyze.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalyzedTrail {
    /// CloudTrail holds a trail under the ARN.
    Present,
    /// No trail answers to the ARN: CloudTrail reported none under it, or the ARN names no
    /// trail at all.
    Absent,
    /// The trail could not be resolved — an emulator assembled with no CloudTrail to ask,
    /// or any other failure to ask it. NOTHING is then established about the trail, so it
    /// grounds no refusal.
    Unresolved,
}

/// What CloudTrail reports about the trail `cloud_trail_arn` names, asked from the
/// `region` the calling request was signed for.
pub async fn resolve_trail(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    cloud_trail_arn: &str,
) -> AnalyzedTrail {
    let region = ::service_common::arn::region_label(region).unwrap_or(super::DEFAULT_REGION);
    match cloud_trail_client(ctx, region)
        .get_trail()
        .name(cloud_trail_arn)
        .send()
        .await
    {
        // CloudTrail holds the trail. Whether it reported the trail's own settings decides
        // nothing here: the question was whether it has one.
        Ok(_) => AnalyzedTrail::Present,
        // CloudTrail says no trail of this account answers to the ARN — either it holds
        // none under it, or the identifier is in no form a trail is named by. The refusal
        // is grounded in its answer.
        Err(error) if reported_absent(&error) => AnalyzedTrail::Absent,
        // Any other answer is this emulator failing to ask, not CloudTrail reporting on the
        // trail, so it refuses nothing — and it is worth a warning, because a deployment
        // with no CloudTrail to ask is one where this resolution does not run. The caller's
        // own value is rendered in its escaped `Debug` form, so a trail ARN carrying a
        // newline cannot forge a log line.
        Err(error) => {
            ::tracing::warn!(
                cloud_trail_arn = ?cloud_trail_arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "a policy generation's trail could not be resolved against CloudTrail"
            );
            AnalyzedTrail::Unresolved
        }
    }
}

/// Whether a CloudTrail `GetTrail` failure is CloudTrail reporting that no trail answers to
/// the identifier — either because it holds none under it, or because the identifier is in
/// no form a trail is named by — as opposed to the call not reaching CloudTrail at all.
fn reported_absent(
    error: &::aws_sdk_cloudtrail::error::SdkError<
        ::aws_sdk_cloudtrail::operation::get_trail::GetTrailError,
        ::aws_smithy_runtime_api::client::orchestrator::HttpResponse,
    >,
) -> bool {
    error.as_service_error().is_some_and(|service_error| {
        service_error.is_trail_not_found_exception()
            || service_error.is_cloud_trail_arn_invalid_exception()
            || service_error.is_invalid_trail_name_exception()
    })
}

/// A CloudTrail client over the in-process callback into this same emulator, addressed to
/// `region`'s endpoint.
///
/// The transport is in-process, so there is nothing to retry, time out or watch for a
/// stalled stream — each would need a sleep implementation this client is not given — and
/// the credentials the callback hands over are used directly rather than cached.
fn cloud_trail_client(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_cloudtrail::Client {
    let config = ::aws_sdk_cloudtrail::Config::builder()
        .behavior_version(::aws_sdk_cloudtrail::config::BehaviorVersion::latest())
        .region(::aws_sdk_cloudtrail::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://cloudtrail.{region}.amazonaws.com"))
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .http_client(ctx.callback().http_client())
        .retry_config(::aws_sdk_cloudtrail::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_cloudtrail::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_cloudtrail::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_cloudtrail::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_cloudtrail::Client::from_conf(config)
}

/// The trail NAME a trail ARN carries: the part after its `trail/` resource type, which is
/// how CloudTrail itself names a trail and the part a client has to correct.
///
/// `None` for a value that is no trail ARN at all — which the modelled pattern on
/// `Trail$cloudTrailArn` already refuses at the request boundary.
pub fn trail_name(cloud_trail_arn: &str) -> Option<&str> {
    let resource = cloud_trail_arn
        .splitn(ARN_FIELDS_BEFORE_RESOURCE + 1, ':')
        .nth(ARN_FIELDS_BEFORE_RESOURCE)?;
    let name = resource.strip_prefix(TRAIL_RESOURCE_PREFIX)?;
    (!name.is_empty()).then_some(name)
}

/// The `ValidationException` for a `cloudTrailArn` naming a trail CloudTrail does not hold.
///
/// AWS words this refusal as one sentence about that ARN — `Invalid cloudTrailArn: `, then
/// `. Unknown trail: `, then ` for the user: ` — so its three variable parts are the ARN
/// exactly as the request sent it, the trail name that ARN carries ([`trail_name`]) and the
/// account the call was made for. Nothing of it is fixed to one trail or one account.
pub fn unknown_trail_exception(cloud_trail_arn: &str) -> ValidationException {
    // An ARN carrying no trail name is one CloudTrail names no trail by either; the
    // refusal then repeats the identifier where the name would go rather than leaving the
    // sentence with a hole in it.
    let name = trail_name(cloud_trail_arn).unwrap_or(cloud_trail_arn);
    super::field_validation_exception(
        TRAIL_ARN_MEMBER,
        format!(
            "Invalid cloudTrailArn: {cloud_trail_arn}. Unknown trail: {name} for the user: {}",
            ::engine::emulator_account_id()
        ),
    )
}

/// The `ValidationException` for a request naming more trails than one start resolves.
///
/// The emulator's own ceiling on caller-driven work, worded the way this crate words every
/// such refusal: the count the request carried and the ceiling it passed.
pub fn too_many_trails_exception(named: usize) -> ValidationException {
    super::field_validation_exception(
        TRAILS_MEMBER,
        format!(
            "cloudTrailDetails.trails names {named} trails, more than the \
             {MAX_RESOLVED_TRAILS} this emulator resolves for one policy generation"
        ),
    )
}

/// The member a trail-existence refusal blames, as the request nests it: the
/// `cloudTrailArn` of an entry of `cloudTrailDetails.trails`.
const TRAIL_ARN_MEMBER: &str = "cloudTrailDetails.trails.cloudTrailArn";

#[cfg(test)]
mod tests {
    use super::trail_name;

    /// RULE: the trail name a refusal reports is the part of the ARN after its `trail/`
    /// resource type. The rows span what the reading generalises over: the partitions the
    /// pattern admits, a name carrying the separators a trail name may hold, an ARN whose
    /// resource is no trail, and one carrying no name after the type.
    #[test]
    fn a_trail_arn_names_its_trail() {
        let rows: [(&str, Option<&str>); 6] = [
            ("arn:aws:cloudtrail:us-east-1:000000000000:trail/audit", Some("audit")),
            (
                "arn:aws-us-gov:cloudtrail:us-gov-west-1:000000000000:trail/audit",
                Some("audit"),
            ),
            (
                "arn:aws:cloudtrail:eu-west-3:000000000000:trail/org.audit-2026_q1",
                Some("org.audit-2026_q1"),
            ),
            (
                "arn:aws:cloudtrail:us-east-1:000000000000:eventdatastore/EDS",
                None,
            ),
            ("arn:aws:cloudtrail:us-east-1:000000000000:trail/", None),
            ("not-an-arn", None),
        ];
        for (arn, expected) in rows {
            let read = trail_name(arn);
            assert_eq!(
                read, expected,
                "case {arn:?}: expected {expected:?}, got {read:?}"
            );
        }
    }
}
