// SPDX-License-Identifier: CC0-1.0

//! What has to hold before the account's trace segment destination is switched —
//! the ONE statement of the two preconditions `UpdateTraceSegmentDestination`
//! takes, so the operation's handler keeps only the envelope it refuses with.
//!
//! The destination is an account-global SINGLETON whose switch is asynchronous
//! (`TraceSegmentDestinationStatus` is "PENDING | ACTIVE"), and two things about
//! a requested switch decide whether it can be applied at all:
//!
//! 1. it has to BE a switch. A request naming the destination the account is
//!    already on changes nothing, so there is no transition for the caller to
//!    wait on, and the service says so rather than reporting a second one
//!    ([`destination_already_set_message`]);
//! 2. the destination has to be one this service can DELIVER to. Pointed at
//!    CloudWatch Logs, X-Ray writes each ingested span into the
//!    [`SPANS_LOG_GROUP`] log group of that service ([`super::span_delivery`])
//!    under its OWN service principal ([`XRAY_SERVICE_PRINCIPAL`]) rather than
//!    as the caller, so the spans arrive only where that principal may put log
//!    events ([`SPANS_DELIVERY_ACTION`]) — which is what a CloudWatch Logs
//!    resource policy grants or withholds ([`spans_delivery_is_ungranted`]).
//!
//! The policies are resolved in the CloudWatch Logs service's own store, reached
//! through the SAME shared engine this service's handlers reach their own store
//! through — the store `logs:PutResourcePolicy` writes and
//! `logs:DescribeResourcePolicies` reports — so what this reads is what that
//! service's own readers answer, and the two cannot disagree about which grants
//! are in force. LOCK ORDER: the caller releases this service's own guard before
//! asking, which is what keeps a cycle between the two stores unreachable (the
//! same order [`super::span_delivery`] delivers under).
//!
//! The documents are evaluated by the shared IAM evaluation
//! (`service_common::iam_authorization`), so this service and every other one
//! handed the same document decide alike, and a grant is only read as WITHHELD
//! where the documents say so ([`withholds_delivery`]).

use ::service_com_amazonaws_cloudwatchlogs_logs_20140328::state as logs;
use ::service_common::iam_authorization::{self as iam, Decision};

use super::span_delivery::SPANS_LOG_GROUP;

/// The service principal X-Ray delivers spans under, and so the principal the
/// spans log group's resource policy has to grant: the events are written BY
/// X-Ray rather than by the caller that configured the destination, which is why
/// a resource policy is what admits them.
pub const XRAY_SERVICE_PRINCIPAL: &str = "xray.amazonaws.com";

/// The CloudWatch Logs API X-Ray calls to put a span into [`SPANS_LOG_GROUP`],
/// which is what a refusal names.
pub const SPANS_DELIVERY_API: &str = "PutLogEvents";

/// [`SPANS_DELIVERY_API`] as the IAM action a policy statement grants it by.
pub const SPANS_DELIVERY_ACTION: &str = "logs:PutLogEvents";

/// The suffix the WILDCARD form of a log group's ARN carries (`LogGroup$arn`,
/// which names the group's log streams); the plain form (`LogGroup$logGroupArn`)
/// carries none. A grant may be written on either, so both are asked.
const LOG_GROUP_STREAMS_SUFFIX: &str = ":*";

/// What the service answers a request naming the destination the account is
/// already on.
///
/// Its one variable part is that destination, which is the fact the caller acts
/// on: there was nothing to switch.
#[must_use]
pub fn destination_already_set_message(destination: &str) -> String {
    format!("The destination is already set to {destination}")
}

/// What the service answers a switch to the CloudWatch Logs destination while no
/// resource policy lets it deliver there.
///
/// The sentence states the condition that held — X-Ray may not put events into
/// the log group the spans go to — and names the group that policy belongs to and
/// the API call it has to cover, because the policy to write is the policy OF one
/// group.
#[must_use]
pub fn spans_delivery_ungranted_message() -> String {
    format!(
        "XRay does not have permission to call {SPANS_DELIVERY_API} on the {SPANS_LOG_GROUP} \
         Log Group. Please verify that your CloudWatch Logs Resource Policies are configured \
         correctly"
    )
}

/// Whether the CloudWatch Logs resource policies in force in `region` WITHHOLD
/// [`SPANS_DELIVERY_ACTION`] on [`SPANS_LOG_GROUP`] from
/// [`XRAY_SERVICE_PRINCIPAL`] — the one reading the refusal of a switch to the
/// CloudWatch Logs destination is grounded in.
///
/// The policies in force are the one attached to the spans log group, when the
/// account already holds that group, and the ACCOUNT-scoped ones, which cover
/// the log groups their own statements name — the scope the grant for this
/// feature is written at, since the group is vended by the first delivery and so
/// need not exist when the policy is put.
///
/// `false` when the engine hands out no CloudWatch Logs store: a precondition
/// that was never readable is no evidence of a missing grant, so nothing is
/// refused on it.
pub fn spans_delivery_is_ungranted<Eng: Send + Sync + 'static>(engine: &Eng, region: &str) -> bool {
    let Some(state) = logs::service_state(engine) else {
        return false;
    };
    let store = state.lock();
    // Read once each, since every resource spelling below is decided against the
    // same set.
    let documents: Vec<iam::PreparedDocument> = store
        .log_groups
        .get(SPANS_LOG_GROUP)
        .and_then(|group| group.resource_policy.as_ref())
        .map(|policy| policy.policy_document.as_str())
        .into_iter()
        .chain(
            store
                .account_resource_policies
                .values()
                .map(|policy| policy.policy_document.as_str()),
        )
        .map(iam::PreparedDocument::read)
        .collect();
    let group_arn = logs::log_group_arn(region, SPANS_LOG_GROUP, false);
    withholds_delivery(&documents, &group_arn)
}

/// Whether `documents` withhold [`SPANS_DELIVERY_ACTION`] on the log group `arn`
/// from [`XRAY_SERVICE_PRINCIPAL`].
///
/// A grant may be written on the GROUP or on the group's log streams (the ARN's
/// wildcard form, which is the resource the events are put into), so both
/// spellings are asked and either one granting the action is a grant of it.
///
/// An explicit `Deny` withholds the action, and so does a set of documents that
/// states nothing about it at all: a policy silent about this principal grants it
/// nothing, which is exactly the case the service refuses. A statement that IS
/// about the delivery and hinges on a condition key this emulator carries no
/// value for leaves the grant undecided, and an undecided grant is no evidence
/// that the real service would refuse the switch — so nothing is withheld on it.
fn withholds_delivery(documents: &[iam::PreparedDocument], arn: &str) -> bool {
    let principals = [XRAY_SERVICE_PRINCIPAL.to_string()];
    let streams = format!("{arn}{LOG_GROUP_STREAMS_SUFFIX}");
    let mut allowed = false;
    let mut undecided = false;
    for resource in [arn, streams.as_str()] {
        let request = iam::Request::new(SPANS_DELIVERY_ACTION, resource, &principals);
        for document in documents {
            let outcome = iam::resource_outcome_prepared(document, &request);
            match outcome.decision {
                Decision::Allowed => allowed = true,
                Decision::Denied => return true,
                Decision::NotStated => {
                    undecided = undecided || !outcome.required_context_keys.is_empty();
                }
            }
        }
    }
    !allowed && !undecided
}
