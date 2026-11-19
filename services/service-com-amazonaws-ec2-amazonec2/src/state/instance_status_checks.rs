// SPDX-License-Identifier: CC0-1.0

//! The STATUS CHECKS one instance reports: "Amazon EC2 performs status checks on running EC2
//! instances to identify hardware and software issues" (`DescribeInstanceStatus`).
//!
//! # What a check reports, and when
//!
//! EC2 reports two of them per instance — the SYSTEM status, which is the host the instance
//! runs on, and the INSTANCE status, which is the instance's own software and network stack —
//! each as a summary (`SummaryStatus`: `ok | impaired | initializing | insufficient-data |
//! not-applicable`) with the detail it was decided from (`StatusName` is `reachability`, and
//! `StatusType` is `passed | failed | initializing | insufficient-data`).
//!
//! A check is owed to the CLOCK, not to a call: an instance whose operating system has just
//! come up is still `initializing` — "it can take a few minutes for the instance to pass its
//! status checks" — and passes [`STATUS_CHECK_SPAN`] after that boot, whether or not anybody
//! polls. The instant it is measured from is the boot the guest actually had
//! ([`super::instance_boots`]), so two reads at one instant answer the same status and
//! reading a status writes nothing. An instance that is still STARTING has no boot to
//! measure from yet, so its checks are initializing too.
//!
//! An instance whose operating system is neither up nor coming up has no check to report at
//! all: "by default, only running instances are described", and the status of one that is
//! included anyway (through `IncludeAllInstances`) is `not-applicable` — with NO detail
//! beside it, because `StatusType` models no value for a check that does not apply.

use std::time::{Duration, SystemTime};

use super::{
    zone_id_for_zone_name, ComAmazonawsEc2Amazonec2State, InstanceRecord, InstanceStateName,
    TRANSITION_DELAY,
};
use crate::ops::types::{
    InstanceStatus, InstanceStatusDetails, InstanceStatusEvent, InstanceStatusSummary,
};

/// How long after its guest came up an instance's status checks PASS.
///
/// AWS takes "a few minutes" and publishes no figure, so the span is this emulator's own —
/// the one every clock-owed move of this crate uses ([`TRANSITION_DELAY`]), which is never
/// zero, so a caller that polls straight after a launch or a reboot still sees the
/// `initializing` state the real service reports.
pub const STATUS_CHECK_SPAN: Duration = TRANSITION_DELAY;

/// `SummaryStatus`'s `ok` — the checks passed.
pub const SUMMARY_STATUS_OK: &str = "ok";

/// `SummaryStatus`'s `initializing` — the checks have not finished yet.
pub const SUMMARY_STATUS_INITIALIZING: &str = "initializing";

/// `SummaryStatus`'s `impaired` — the check FAILED, which is what an instance or a host with a
/// problem reports. Nothing in this store impairs an instance or its host, so no check here
/// reaches it; it is named beside the other members of the catalog so the one place that reads a
/// check's verdict ([`super::instance_telemetry`]) spells every verdict that catalog has rather
/// than only the ones this store produces.
pub const SUMMARY_STATUS_IMPAIRED: &str = "impaired";

/// `SummaryStatus`'s `not-applicable` — there is nothing to check, which is what an instance
/// that is not running reports.
pub const SUMMARY_STATUS_NOT_APPLICABLE: &str = "not-applicable";

/// The one name `StatusName` models for a status detail.
pub const STATUS_NAME_REACHABILITY: &str = "reachability";

/// `StatusType`'s `passed`.
pub const STATUS_TYPE_PASSED: &str = "passed";

/// `StatusType`'s `initializing`.
pub const STATUS_TYPE_INITIALIZING: &str = "initializing";

/// The filters `DescribeInstanceStatus` documents, as their names.
///
/// From that operation's own `Filters` doc — the whole list, so a name it documents is never
/// refused and a name it does not is never silently ignored. The ones this emulator holds no
/// fact for (a scheduled event, an attached-volume or application status, a managing
/// principal) resolve to no value, so a request filtering on one selects nothing rather than
/// everything.
pub const SUPPORTED_INSTANCE_STATUS_FILTERS: &[&str] = &[
    "availability-zone",
    "availability-zone-id",
    "event.code",
    "event.description",
    "event.instance-event-id",
    "event.not-after",
    "event.not-before",
    "event.not-before-deadline",
    "instance-state-code",
    "instance-state-name",
    "instance-status.reachability",
    "instance-status.status",
    "operator.managed",
    "operator.principal",
    "system-status.reachability",
    "system-status.status",
    "attached-ebs-status.status",
    "application-status.status",
];

/// Whether `name` is a filter `DescribeInstanceStatus` documents.
pub fn instance_status_filter_supported(name: &str) -> bool {
    SUPPORTED_INSTANCE_STATUS_FILTERS.contains(&name)
}

/// The status checks `instance` reports as of `now`: `(system status, instance status)`.
///
/// Both halves answer the same way, because this emulator runs one host per instance: the
/// checks of an instance whose operating system is neither up nor coming up do not apply,
/// those of a guest that came up less than [`STATUS_CHECK_SPAN`] ago — or has not come up
/// yet — are still initializing, and those of a guest that came up before that have passed.
pub fn instance_status_checks(
    instance: &InstanceRecord,
    now: SystemTime,
) -> (InstanceStatusSummary, InstanceStatusSummary) {
    let summary = if !matches!(
        instance.state,
        InstanceStateName::Running | InstanceStateName::Pending
    ) {
        InstanceStatusSummary {
            // No detail: `StatusType` models no value for a check that does not apply, so a
            // detail here would have to carry a status the API does not publish.
            details: None,
            status: Some(SUMMARY_STATUS_NOT_APPLICABLE.to_string()),
        }
    } else if checks_have_passed(instance, now) {
        reachability_summary(SUMMARY_STATUS_OK, STATUS_TYPE_PASSED)
    } else {
        reachability_summary(SUMMARY_STATUS_INITIALIZING, STATUS_TYPE_INITIALIZING)
    };
    (summary.clone(), summary)
}

/// Whether the checks of `instance` have passed at `now`: its guest came up at least
/// [`STATUS_CHECK_SPAN`] ago.
///
/// An instance whose guest has not come up at all is still initializing — there is nothing to
/// have passed yet.
fn checks_have_passed(instance: &InstanceRecord, now: SystemTime) -> bool {
    super::instance_boots(instance)
        .last()
        .and_then(|boot| boot.booted_at.checked_add(STATUS_CHECK_SPAN))
        .is_some_and(|due| now >= due)
}

/// One summary of the reachability check: the summary status and the detail it was decided
/// from.
fn reachability_summary(status: &str, detail: &str) -> InstanceStatusSummary {
    InstanceStatusSummary {
        details: Some(vec![InstanceStatusDetails {
            // "The time when a status check failed" (`$ImpairedSince`): nothing here has
            // failed, so no instant is reported.
            impaired_since: None,
            name: Some(STATUS_NAME_REACHABILITY.to_string()),
            status: Some(detail.to_string()),
        }]),
        status: Some(status.to_string()),
    }
}

impl ComAmazonawsEc2Amazonec2State {
    /// The `InstanceStatus` `instance` reports as of `now`.
    ///
    /// Every member is read off the stored instance: its id, the state it is in, the zone it
    /// is placed in (and that zone's id, through the one mapping this crate holds) and the
    /// checks above. A member this emulator holds no fact for is absent rather than filled
    /// in: no scheduled event is modelled, so `events` is empty; no attached-volume,
    /// application or performance status is measured, so none is claimed.
    pub fn instance_status_view(&self, instance: &InstanceRecord, now: SystemTime) -> InstanceStatus {
        let (system_status, instance_status) = instance_status_checks(instance, now);
        InstanceStatus {
            availability_zone: Some(instance.placement_az.clone()),
            availability_zone_id: zone_id_for_zone_name(&instance.placement_az),
            // An Outpost is a resource this emulator does not hold, so no instance is
            // reported on one.
            outpost_arn: None,
            operator: Some(super::unmanaged_operator()),
            // The scheduled events of this instance, through the one reading of them
            // ([`scheduled_instance_events`]): absent for an instance with none, which the
            // shape's own optionality is for.
            events: Some(scheduled_instance_events(instance))
                .filter(|events| !events.is_empty()),
            instance_id: Some(instance.instance_id.clone()),
            instance_state: Some(instance.state.view()),
            instance_status: Some(instance_status),
            system_status: Some(system_status),
            attached_ebs_status: None,
            application_status: None,
            performance_status: None,
        }
    }
}

/// The SCHEDULED EVENTS one instance reports: "any scheduled events associated with the
/// instance" (`InstanceStatus$Events`).
///
/// Amazon EC2 schedules these itself, for maintenance it has planned on the host an instance
/// runs on — "scheduled events for your instances" are created by EC2, and no API of this
/// service creates one: `ModifyInstanceEventStartTime` moves the start time of one EC2 has
/// already scheduled, and `DescribeInstanceStatus` reports it. Nothing in this emulator plans
/// maintenance, so no instance of this store has an event scheduled against it, and an event
/// is never invented for one: an instance reports the events it HAS, which is none.
///
/// The ONE reading of them, so the describe that reports an instance's events and the modify
/// that moves one cannot disagree about which events an instance has.
pub fn scheduled_instance_events(instance: &InstanceRecord) -> Vec<InstanceStatusEvent> {
    let _ = instance;
    Vec::new()
}

/// The scheduled event of `instance` that `instance_event_id` names, if it has one.
///
/// Read off [`scheduled_instance_events`], so an operation that acts on ONE event resolves it
/// against exactly the events the describe reports for that instance.
pub fn scheduled_instance_event(
    instance: &InstanceRecord,
    instance_event_id: &str,
) -> Option<InstanceStatusEvent> {
    scheduled_instance_events(instance)
        .into_iter()
        .find(|event| event.instance_event_id.as_deref() == Some(instance_event_id))
}

/// The code EC2 answers for an instance event id the instance it was asked about has no
/// scheduled event under.
///
/// The not-found half of the resource-id pair this service answers for every other kind of
/// id: the id resolved to no event of that instance, so retrying it unchanged can only fail
/// again. Spelt WITHOUT the dot every other not-found code of this service carries —
/// `ModifyInstanceEventStartTime` is recorded answering exactly this word — so it is named
/// here once rather than assembled from the pattern the other id kinds follow.
pub const INVALID_INSTANCE_EVENT_ID_NOT_FOUND: &str = "InvalidInstanceEventIDNotFound";

/// Refuse a request naming an instance event the instance has no scheduled event for: HTTP
/// 400 [`INVALID_INSTANCE_EVENT_ID_NOT_FOUND`].
///
/// AWS's sentence for this class is FIXED text end to end — `Cannot find event`, naming
/// neither the event nor the instance — which is what `ModifyInstanceEventStartTime` is
/// recorded answering, so every operation that cannot find a scheduled event answers exactly
/// this one.
pub fn invalid_instance_event_id_not_found<E>() -> crate::scaffolded::Scaffolded<E> {
    crate::scaffolded::Scaffolded::service_error(
        INVALID_INSTANCE_EVENT_ID_NOT_FOUND,
        400,
        "Cannot find event",
    )
}

/// The values one instance holds for the `DescribeInstanceStatus` filter `name`, or `None`
/// when that name is no filter of this operation.
///
/// An empty list is a filter the instance holds NO value for — a scheduled event this
/// emulator does not model, a status it does not measure — so a request filtering on it
/// selects nothing, which is what a filter on a fact an instance does not have means.
pub fn instance_status_filter_values(
    status: &InstanceStatus,
    name: &str,
) -> Option<Vec<String>> {
    let summary_status = |summary: &Option<InstanceStatusSummary>| {
        summary
            .as_ref()
            .and_then(|summary| summary.status.clone())
            .into_iter()
            .collect::<Vec<String>>()
    };
    let reachability = |summary: &Option<InstanceStatusSummary>| {
        summary
            .as_ref()
            .and_then(|summary| summary.details.clone())
            .into_iter()
            .flatten()
            .filter(|detail| detail.name.as_deref() == Some(STATUS_NAME_REACHABILITY))
            .filter_map(|detail| detail.status)
            .collect::<Vec<String>>()
    };
    let held = match name {
        "availability-zone" => status.availability_zone.clone().into_iter().collect(),
        "availability-zone-id" => status.availability_zone_id.clone().into_iter().collect(),
        "instance-state-code" => status
            .instance_state
            .as_ref()
            .and_then(|state| state.code)
            .map(|code| code.to_string())
            .into_iter()
            .collect(),
        "instance-state-name" => status
            .instance_state
            .as_ref()
            .and_then(|state| state.name.clone())
            .into_iter()
            .collect(),
        "instance-status.status" => summary_status(&status.instance_status),
        "instance-status.reachability" => reachability(&status.instance_status),
        "system-status.status" => summary_status(&status.system_status),
        "system-status.reachability" => reachability(&status.system_status),
        "attached-ebs-status.status" => status
            .attached_ebs_status
            .as_ref()
            .and_then(|summary| summary.status.clone())
            .into_iter()
            .collect(),
        "application-status.status" => status
            .application_status
            .as_ref()
            .and_then(|summary| summary.status.clone())
            .into_iter()
            .collect(),
        "operator.managed" => status
            .operator
            .as_ref()
            .and_then(|operator| operator.managed)
            .map(|managed| managed.to_string())
            .into_iter()
            .collect(),
        "operator.principal" => status
            .operator
            .as_ref()
            .and_then(|operator| operator.principal.clone())
            .into_iter()
            .collect(),
        // Every scheduled-event filter: this emulator schedules no event, so an instance
        // holds no value for one.
        "event.code"
        | "event.description"
        | "event.instance-event-id"
        | "event.not-after"
        | "event.not-before"
        | "event.not-before-deadline" => Vec::new(),
        _ => return None,
    };
    Some(held)
}
