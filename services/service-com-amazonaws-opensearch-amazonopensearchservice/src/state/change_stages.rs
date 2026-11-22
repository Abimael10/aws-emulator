// SPDX-License-Identifier: CC0-1.0

//! The STAGES of a configuration change the service is applying to a domain's
//! provisioned cluster — what `DescribeDomainChangeProgress` reports as
//! `ChangeProgressStatusDetails$ChangeProgressStages`, "the specific stages that the
//! domain is going through to perform the configuration change", beside the
//! `TotalNumberOfStages` that list is the length of.
//!
//! A change the service has BEGUN APPLYING to a cluster is applied by deploying that
//! cluster anew, so the record of the change carries the sequence of phases that
//! deployment goes through ([`CHANGE_STAGES`]), each with the span of the change's own
//! provisioning window it is worked on in. The clock is what advances them: a stage
//! whose span has begun is being worked on and one whose span has passed is done, so a
//! caller polling the change watches the stages advance without anything having to be
//! scheduled — the same rule [`ConfigChange::settle`](super::ConfigChange::settle)
//! follows for the change as a whole.
//!
//! A change the service has not begun applying — the one a create starts, before there
//! is a cluster to apply anything to — goes through no such deployment, so it records
//! no stage and reports an empty list.
//!
//! The stage list is MATERIALISED on the change rather than derived from its window at
//! each read — which is how an engine version upgrade's steps are reported
//! ([`UpgradeRun::step_progress`](super::UpgradeRun::step_progress)) — because a stage
//! is not a pure function of the clock: a cancel stops the deployment where it found
//! it, so a stage already performed stays performed while an unperformed one is left
//! reported as not going to be ([`ChangeStage::fail`]), which the window alone cannot
//! say.

use super::*;

/// `ChangeProgressStage$Status` for a stage the service has not reached yet.
///
/// `ChangeProgressStage$Status` is a free `String` of 1..256 characters in this
/// service's model; the value set is the one the ingestion service's model publishes
/// as an enum for the same concept (`ChangeProgressStageStatuses`): `PENDING`,
/// `IN_PROGRESS`, `COMPLETED` and `FAILED`.
pub const STAGE_STATUS_PENDING: &str = "PENDING";

/// `ChangeProgressStage$Status` for the stage being worked on right now.
pub const STAGE_STATUS_IN_PROGRESS: &str = "IN_PROGRESS";

/// `ChangeProgressStage$Status` for a stage that has been performed.
pub const STAGE_STATUS_COMPLETED: &str = "COMPLETED";

/// `ChangeProgressStage$Status` for a stage that will not be performed, because the
/// change it belongs to was stopped before it got there.
pub const STAGE_STATUS_FAILED: &str = "FAILED";

/// The phases the service goes through to apply a configuration change to a running
/// cluster, in the order it performs them: a new environment is provisioned, the
/// domain's data is copied onto it and the domain is switched over to it — the
/// deployment `DescribeDomainChangeProgress` is documented to report the progress of.
///
/// Each entry is a stage's `Name` (within the member's own 1..64 bound) and its
/// `Description`. The names the service itself publishes for them are not in the
/// model, so the phases are named for the work each performs.
pub const CHANGE_STAGES: [(&str, &str); 4] = [
    (
        "Validating",
        "Validating the configuration change against the domain.",
    ),
    (
        "Creating a new environment",
        "Provisioning the nodes of the new environment the change is applied on.",
    ),
    (
        "Copying data",
        "Copying the domain's shards onto the new environment.",
    ),
    (
        "Switching over",
        "Switching the domain over to the new environment and releasing the old one.",
    ),
];

/// How many stages that is — the denominator the change's provisioning window is
/// divided into shares by, and the `TotalNumberOfStages` a change going through them
/// reports.
pub const STAGE_COUNT: u32 = CHANGE_STAGES.len() as u32;

/// One stage of a configuration change: the work it performs, the span of the change's
/// provisioning window it is performed in, and where it has got to.
///
/// The span is what the clock advances the stage over, so the status a caller reads is
/// never ahead of or behind the change it belongs to: the first stage is being worked
/// on from the instant the change is admitted and the last one is done at the instant
/// the change itself completes.
#[derive(Debug, Clone)]
pub struct ChangeStage {
    /// `ChangeProgressStage$Name`: which phase of the deployment this is, from
    /// [`CHANGE_STAGES`].
    pub name: String,
    /// `ChangeProgressStage$Description`: what that phase does.
    pub description: String,
    /// When the service is due to START this stage — the instant it stops being
    /// [`STAGE_STATUS_PENDING`].
    pub begins_at: SystemTime,
    /// When the service is due to have FINISHED it.
    pub ends_at: SystemTime,
    /// `ChangeProgressStage$Status`, one of the four values above.
    pub status: String,
    /// `ChangeProgressStage$LastUpdated`: when this stage's status last moved.
    pub last_updated_at: SystemTime,
}

impl ChangeStage {
    /// Materialise whatever the clock owes this stage at `now`: it is being worked on
    /// once its span has begun and done once its span has passed.
    ///
    /// Each movement is dated at the instant it fell DUE rather than at `now`, so the
    /// `LastUpdated` a caller reads does not move with the moment it happened to look.
    /// A stage a cancel stopped ([`Self::fail`]) is terminal and moves no further: the
    /// work it stood for is not going to be performed.
    pub(super) fn settle(&mut self, now: SystemTime) {
        if self.status == STAGE_STATUS_FAILED {
            return;
        }
        let (status, moved_at) = if now >= self.ends_at {
            (STAGE_STATUS_COMPLETED, self.ends_at)
        } else if now >= self.begins_at {
            (STAGE_STATUS_IN_PROGRESS, self.begins_at)
        } else {
            return;
        };
        if self.status != status {
            self.status = status.to_string();
            self.last_updated_at = moved_at;
        }
    }

    /// Stop this stage at `now`: the change it belongs to was cancelled, so a stage it
    /// had not finished is not going to be performed. A stage already performed stays
    /// performed — cancelling a change does not unmake the work already done for it.
    pub(super) fn fail(&mut self, now: SystemTime) {
        if self.status == STAGE_STATUS_COMPLETED {
            return;
        }
        self.status = STAGE_STATUS_FAILED.to_string();
        self.last_updated_at = now;
    }

    /// This stage as the modelled `ChangeProgressStage`.
    pub fn to_stage(&self) -> ChangeProgressStage {
        ChangeProgressStage {
            name: Some(self.name.clone()),
            status: Some(self.status.clone()),
            description: Some(self.description.clone()),
            last_updated: Some(datetime(self.last_updated_at)),
        }
    }
}

/// The stage sequence of a change admitted at `started_at` that the clock owes
/// completion at `completes_at`: [`CHANGE_STAGES`] in order, each worked on in an
/// equal share of that window, with the share already reached materialised.
///
/// The shares partition the change's own window, so the stages can neither finish
/// before the change does nor outlive it: the first is being worked on the instant the
/// change is admitted — a caller polling straight afterwards reads a stage in flight,
/// not a list of stages that have not started — and the last finishes when the change
/// completes.
pub fn stage_plan(started_at: SystemTime, completes_at: SystemTime) -> Vec<ChangeStage> {
    let window = completes_at
        .duration_since(started_at)
        .unwrap_or(DOMAIN_WORK);
    let share = window / STAGE_COUNT;
    let mut stages: Vec<ChangeStage> = CHANGE_STAGES
        .iter()
        .zip(0u32..)
        .map(|((name, description), steps)| {
            let offset = |steps: u32| {
                started_at
                    .checked_add(share.saturating_mul(steps))
                    .unwrap_or(completes_at)
            };
            ChangeStage {
                name: (*name).to_string(),
                description: (*description).to_string(),
                begins_at: offset(steps),
                // The last stage ends when the change itself does, whatever rounding
                // dividing the window into shares left over.
                ends_at: if steps + 1 == STAGE_COUNT {
                    completes_at
                } else {
                    offset(steps + 1)
                },
                status: STAGE_STATUS_PENDING.to_string(),
                last_updated_at: started_at,
            }
        })
        .collect();
    for stage in &mut stages {
        stage.settle(started_at);
    }
    stages
}
