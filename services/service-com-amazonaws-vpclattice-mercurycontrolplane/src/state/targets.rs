// SPDX-License-Identifier: CC0-1.0

//! The target slice of this service's state: what a registered target REPORTS, which
//! targets a registration ADMITS, and what a deregistration does to the stored records.
//!
//! Three readings live here, each shared by every operation of the slice
//! (`RegisterTargets`, `DeregisterTargets`, `ListTargets`):
//!
//! * [`ComAmazonawsVpclatticeMercurycontrolplaneState::target_health`] — the `TargetStatus`
//!   a target reports, together with the `reasonCode` that says which case that status is.
//!   The health-check condition is read FIRST and independently of whether anything routes
//!   to the target group, because "Health checks are unavailable for the target group"
//!   (`TargetSummary`'s `status`) is a property of the GROUP's own type and configuration,
//!   while `UNUSED` is a property of what routes to it.
//! * [`check_target_owner`] — the per-target admission `RegisterTargets` applies to each
//!   target on its own. A target whose SHAPE this service does not accept is input the
//!   whole request is malformed in ([`super::check_target_id`]); a target of the right
//!   shape that this service cannot register is reported PER TARGET, under the
//!   `unsuccessful` member (`TargetFailureList`) that operation models for exactly that.
//! * [`ComAmazonawsVpclatticeMercurycontrolplaneState::deregister_targets`] — the one
//!   removal path for a registration, with the connection draining `TargetStatus`
//!   documents for a target that is being deregistered, and the per-target failure a
//!   deregistration reports for a target it cannot deregister
//!   ([`TARGET_FAILURE_TARGET_NOT_FOUND`], [`TARGET_FAILURE_DEREGISTER_MESSAGE`]).

use std::time::{Duration, SystemTime};

use super::{
    account_id, config_member_supported, ComAmazonawsVpclatticeMercurycontrolplaneState,
    RegisteredTarget, TargetGroup, TargetIdForm, TARGET_ID_FORM, TARGET_STATUS_HEALTHY,
    TARGET_STATUS_INITIAL, TARGET_STATUS_UNAVAILABLE, TARGET_STATUS_UNUSED,
};

/// The `TargetGroupConfig` member that decides whether a target group health-checks its
/// targets at all, as the model names it.
pub const HEALTH_CHECK_MEMBER: &str = "healthCheck";

/// The `TargetStatus` of a target a deregistration has started removing: "The target is
/// deregistering and connection draining is in process."
/// (`smithy_query(query="shape",shape="TargetStatus")`).
pub const TARGET_STATUS_DRAINING: &str = "DRAINING";

/// The `reasonCode` of the `UNAVAILABLE` status of a target whose target group's TYPE runs
/// no health checks at all.
///
/// SOURCE: recorded AWS behaviour — a target of a target group whose type supports no
/// `healthCheck` configuration ([`super::CONFIG_MEMBER_SUPPORT`]) is reported `UNAVAILABLE`
/// with this code. `reasonCode` is "the code for why the target status is what it is"
/// (`TargetSummary`), typed as a free `String` with no value list, so the token is fixed
/// text for this case.
pub const TARGET_REASON_HEALTH_CHECK_NOT_SUPPORTED: &str = "HealthCheckNotSupported";

/// The `reasonCode` of the `UNAVAILABLE` status of a target whose target group COULD
/// health-check it but has health checking turned off (`HealthCheckConfig`'s `enabled`,
/// "Indicates whether health checking is enabled").
///
/// `UNAVAILABLE` covers two states — "Health checks are unavailable for the target group"
/// (`TargetSummary`'s `status`) and "Health checks are disabled" (`TargetStatus`'s own
/// `UNAVAILABLE`) — and the reason code is what tells a caller which of them it is reading,
/// so the second case carries its own token beside
/// [`TARGET_REASON_HEALTH_CHECK_NOT_SUPPORTED`].
pub const TARGET_REASON_HEALTH_CHECK_DISABLED: &str = "HealthCheckDisabled";

/// How long a target of a target group whose targets are reached over connections reports
/// `DRAINING` before the deregistration drops it.
///
/// SOURCE: `TargetSummary`'s `status` documentation — "`DRAINING`: The target is being
/// deregistered. No new connections are sent to this target while current connections are
/// being drained. The default draining time is 1 minute." Nothing sleeps for it: the due
/// instant is stored on the target and compared on read
/// ([`ComAmazonawsVpclatticeMercurycontrolplaneState::expire_drained_targets`]).
pub const TARGET_DRAINING_TIME: Duration = Duration::from_secs(60);

/// The `failureCode` of a target this service accepts the shape of but cannot register.
///
/// SOURCE: recorded AWS behaviour. `TargetFailure`'s `failureCode` is "the failure code",
/// typed as a free `String` with no value list, so the token is fixed text for this
/// failure class.
pub const TARGET_FAILURE_INVALID_TARGET: &str = "InvalidTarget";

/// The `failureCode` of a target a deregistration named that its target group holds no
/// registration of.
///
/// SOURCE: recorded AWS behaviour. A failure class of its own: the target is not one this
/// service refuses, it is one the target group does not have, so it carries its own token
/// beside [`TARGET_FAILURE_INVALID_TARGET`].
pub const TARGET_FAILURE_TARGET_NOT_FOUND: &str = "TargetNotFound";

/// The `failureMessage` of a target a deregistration could not deregister.
///
/// SOURCE: recorded AWS behaviour. `TargetFailure`'s `failureMessage` is "the failure
/// message", typed as a free `String` the model fixes no text for, and the service answers
/// ONE fixed sentence for this failure class: it interpolates nothing from the request and
/// nothing from service state — no target id, no ARN, no port, no target group — so every
/// target of every target-group type that a deregistration cannot deregister carries
/// exactly this text.
pub const TARGET_FAILURE_DEREGISTER_MESSAGE: &str = "Target failed to deregister.";

/// What a target reports: its `TargetStatus` and the `reasonCode` that says which case
/// that status is (`None` for a status that has only one case).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetHealth {
    /// `TargetSummary.status`.
    pub status: &'static str,
    /// `TargetSummary.reasonCode`: "the code for why the target status is what it is".
    pub reason_code: Option<&'static str>,
}

/// Why this service cannot register ONE target of a registration: the `failureCode` and
/// `failureMessage` the per-target failure carries.
///
/// A rejection of this kind is NOT a request-level refusal: `RegisterTargets` models
/// `unsuccessful` (`TargetFailureList`) beside `successful`, so the targets the service
/// does admit are still stored and the call still answers HTTP 200.
#[derive(Debug, Clone)]
pub struct TargetRejection {
    /// `TargetFailure.failureCode`.
    pub failure_code: String,
    /// `TargetFailure.failureMessage`.
    pub failure_message: String,
}

/// Whether the targets of a target group of `group_type` are reached over CONNECTIONS, so
/// a deregistration of one has connections to drain.
///
/// A target group whose configuration supports a `port` is one whose targets are reached
/// over a connection on that port ([`super::CONFIG_MEMBER_SUPPORT`]); a `LAMBDA` group
/// supports none and its target is a function, which "doesn't apply if the target is a
/// Lambda function" (`TargetFailure`'s `port`) — nothing is connected to it, so there is
/// nothing to drain and the deregistration drops it at once.
pub fn connections_drain(group_type: &str) -> bool {
    config_member_supported(group_type, "port")
}

/// The per-target admission a registration applies to ONE target: `Ok(())` for a target
/// this service can register, and the per-target failure for one it cannot.
///
/// The rule is ownership: a target of a type whose `id` is another service's ARN
/// ([`TARGET_ID_FORM`]) names the account that holds it, and this service registers only a
/// target of the CALLER's own account ([`account_id`]). A target id that carries no account
/// — an instance id, an IP address — names nothing to own, and is admitted.
pub fn check_target_owner(group_type: &str, id: &str) -> Result<(), TargetRejection> {
    let Some(label) = arn_target_label(group_type) else {
        return Ok(());
    };
    let Some(owner) = arn_account(id) else {
        return Ok(());
    };
    let caller = account_id();
    if owner == caller {
        return Ok(());
    }
    Err(TargetRejection {
        failure_code: TARGET_FAILURE_INVALID_TARGET.to_string(),
        failure_message: not_owned_message(label, id, caller),
    })
}

/// How the service reports a target that names a resource of another account.
///
/// SOURCE: recorded AWS behaviour. The fixed text is the label of what the target is, then
/// " is not owned by account " and the trailing period; the variable parts are the target
/// id the request submitted and the caller's own account ([`account_id`]).
fn not_owned_message(label: &str, id: &str, caller: &str) -> String {
    format!("{label} {id} is not owned by account {caller}.")
}

/// What a target of `group_type` IS, when the type's own documentation makes its `id` an
/// ARN: the label a failure names it by (`Target`'s `id`, "If the target group type is
/// `LAMBDA`, this is the ARN of a Lambda function. If the target group type is `ALB`, this
/// is the ARN of an Application Load Balancer"). `None` for a type whose target id is not
/// an ARN.
fn arn_target_label(group_type: &str) -> Option<&'static str> {
    TARGET_ID_FORM
        .iter()
        .find(|(name, _)| *name == group_type)
        .and_then(|(_, form)| match form {
            TargetIdForm::Arn { label, .. } => Some(*label),
            _ => None,
        })
}

/// The account field of an ARN: `arn:<partition>:<service>:<region>:<account>:<resource…>`,
/// so the fifth colon-separated field. `None` when the id is not an ARN or names no
/// account.
fn arn_account(id: &str) -> Option<&str> {
    let account = id.split(':').nth(4)?;
    (!account.is_empty()).then_some(account)
}

impl TargetGroup {
    /// Why health checks do not apply to this target group's targets, as the `reasonCode`
    /// of their `UNAVAILABLE` status; `None` for a target group that health-checks them.
    ///
    /// Two states report `UNAVAILABLE`: a type that supports no `healthCheck`
    /// configuration at all ([`super::CONFIG_MEMBER_SUPPORT`]: not `LAMBDA`, not `ALB`),
    /// and a configuration that turns health checking off (`enabled`). The reason code is
    /// what tells them apart.
    pub fn health_check_unavailable_reason(&self) -> Option<&'static str> {
        if !config_member_supported(&self.group_type, HEALTH_CHECK_MEMBER) {
            return Some(TARGET_REASON_HEALTH_CHECK_NOT_SUPPORTED);
        }
        if self
            .config
            .health_check
            .as_ref()
            .is_some_and(|check| check.enabled == Some(false))
        {
            return Some(TARGET_REASON_HEALTH_CHECK_DISABLED);
        }
        None
    }
}

impl RegisteredTarget {
    /// Whether a deregistration has started and the clock has made this target's removal
    /// due, so it is no longer registered for any caller.
    pub fn drain_due(&self, now: SystemTime) -> bool {
        self.draining_at.is_some_and(|due| now >= due)
    }
}

impl ComAmazonawsVpclatticeMercurycontrolplaneState {
    /// What one registered target of `group` reports at `now`: its `TargetStatus` and the
    /// `reasonCode` of that status.
    ///
    /// Each branch is the status whose OWN documentation describes the state the stored
    /// records are in (`smithy_query(query="shape",shape="TargetStatus")`), read in the
    /// order the states override one another:
    ///
    /// * `DRAINING` while a deregistration is removing the target ("The target is being
    ///   deregistered");
    /// * `UNAVAILABLE` where the target group runs no health checks — a property of the
    ///   GROUP, so it is reported ahead of and independently of what routes to the group —
    ///   with the reason code that says which `UNAVAILABLE` case it is
    ///   ([`TargetGroup::health_check_unavailable_reason`]);
    /// * `UNUSED` while no listener or rule routes to the target group ("Target group is
    ///   not used in a service", derived from the stored listener and rule records exactly
    ///   as [`Self::service_arns_using`] derives the service list);
    /// * `INITIAL` until the stored due instant of the target's first health checks passes
    ///   ("Initial health checks on the target are being performed"), and `HEALTHY` from
    ///   then on.
    ///
    /// A pure function of stored state and the clock, so reading it changes nothing a
    /// later caller sees.
    pub fn target_health(
        &self,
        group: &TargetGroup,
        target: &RegisteredTarget,
        now: SystemTime,
    ) -> TargetHealth {
        let plain = |status: &'static str| TargetHealth {
            status,
            reason_code: None,
        };
        if target.draining_at.is_some() {
            return plain(TARGET_STATUS_DRAINING);
        }
        if let Some(reason) = group.health_check_unavailable_reason() {
            return TargetHealth {
                status: TARGET_STATUS_UNAVAILABLE,
                reason_code: Some(reason),
            };
        }
        if self.service_arns_using(&group.id).is_empty() {
            return plain(TARGET_STATUS_UNUSED);
        }
        if now < target.healthy_at {
            return plain(TARGET_STATUS_INITIAL);
        }
        plain(TARGET_STATUS_HEALTHY)
    }

    /// Deregister `requested` from the target group under `target_group_id`, answering the
    /// records of the targets that group HELD as they now read (`None` when no target group
    /// is stored under that id).
    ///
    /// A target is its id TOGETHER with the port it is reached on, so an entry naming a
    /// pair the group does not hold changes nothing — which is what makes a retried
    /// deregistration idempotent (`DeregisterTargets` is `@idempotent`).
    ///
    /// A target whose group's targets are reached over connections ([`connections_drain`])
    /// is marked with the instant the clock owes its removal and reports `DRAINING` until
    /// then ([`Self::expire_drained_targets`]); one with no connections to drain is dropped
    /// here. A target already draining keeps the instant its first deregistration set, so a
    /// retry does not extend the drain.
    ///
    /// The requested pairs are indexed ONCE, so the whole deregistration costs one pass
    /// over them plus one over the targets held.
    pub fn deregister_targets(
        &mut self,
        target_group_id: &str,
        requested: &[(String, Option<i32>)],
        now: SystemTime,
    ) -> Option<Vec<RegisteredTarget>> {
        let asked: std::collections::HashSet<(&str, Option<i32>)> = requested
            .iter()
            .map(|(id, port)| (id.as_str(), *port))
            .collect();
        let group = self.target_groups.get_mut(target_group_id)?;
        let draining = connections_drain(&group.group_type);
        let mut deregistered: Vec<RegisteredTarget> = Vec::new();
        for target in &mut group.targets {
            if !asked.contains(&(target.id.as_str(), target.port)) {
                continue;
            }
            if draining {
                target.draining_at = Some(target.draining_at.unwrap_or(now + TARGET_DRAINING_TIME));
            } else {
                // Nothing is connected to this target, so its removal is due at once.
                target.draining_at = Some(now);
            }
            deregistered.push(target.clone());
        }
        // A target with no connections to drain leaves the record with this call, so the
        // listing that follows it reports the membership the deregistration left behind.
        group.targets.retain(|target| !target.drain_due(now));
        Some(deregistered)
    }

    /// Materialise every target removal the clock has already made due: a target whose
    /// connection draining has run out is no longer registered, so it leaves the record it
    /// was registered on.
    ///
    /// Called by the operations that read or write the target slice before they touch it,
    /// so a target is gone for every caller at the same instant — the removal was owed at
    /// that instant whether or not anyone called.
    pub fn expire_drained_targets(&mut self, now: SystemTime) {
        for group in self.target_groups.values_mut() {
            group.targets.retain(|target| !target.drain_due(now));
        }
    }
}
