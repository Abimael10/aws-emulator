// SPDX-License-Identifier: CC0-1.0

//! The EventBridge event a BACKUP PLAN mutation publishes: one per update and one per
//! delete of a plan.
//!
//! # The rule this module implements
//!
//! A backup plan is not only a stored resource a reader can poll: every MUTATION of one
//! that FOLLOWS the call that created it is PUBLISHED onto the account's default
//! EventBridge bus, under the `source` this service owns ([`super::EVENT_SOURCE`]) and the
//! `detail-type` of a plan's own state change ([`BACKUP_PLAN_STATE_CHANGE`]), carrying in
//! the event's `detail` the plan the mutation was performed on — its `backupPlanId` and
//! the plan's other stored members — and the transition it made
//! ([`PLAN_STATE_MODIFIED`] for an update that stored another version of its body,
//! [`PLAN_STATE_DELETED`] for the delete that recorded its deletion), with the plan
//! itself as the resource the event is about in `resources`. A rule on that bus whose
//! `EventPattern` matches then delivers the envelope unchanged to every one of that
//! rule's targets, so a client that drives plan governance off EventBridge — an audit
//! pipeline, a "plan was modified" alarm, a fan-out onto a queue or a topic — observes
//! the mutation without calling AWS Backup again.
//!
//! The plan a CREATE stored is not such a mutation: it is what that call's own response
//! reports, so a client that made it already holds the plan's identifiers and its first
//! version, and the bus carries the moves that follow. This is the same rule the job state
//! changes beside this module follow for the state a start call stored, and it is what the
//! recorded service does: a rule installed BEFORE a create and read after the update that
//! follows it is handed the UPDATE's envelope as the first one naming that plan, which a
//! create's own envelope, queued ahead of it, would contradict.
//!
//! Only a plan's OWN mutations are announced this way. A change to something else about a
//! plan is not one of them: the tags a tagging call writes and the resource selections
//! assigned to it are no new version of the plan body, so they owe no plan state change.
//! Nor does any other resource of this service have a detail-type of its own here: an
//! audit framework's create and update publish nothing at all.
//!
//! # How a mutation becomes an event
//!
//! A DIFF, exactly as the job state changes beside it are
//! ([`super::ComAmazonawsBackupCryocontrollerusermanagerState::record_job_state_changes`]):
//! [`super::ComAmazonawsBackupCryocontrollerusermanagerState::record_backup_plan_state_changes`]
//! compares every stored plan against what was last published for it
//! ([`BackupPlanEventLog`], held in the store beside the plans) and queues one change per
//! transition that differs — the plan whose current version id has moved on, the plan
//! whose deletion is now recorded. A plan seen for the FIRST time only SEEDS that
//! snapshot, which is how the create it was stored by announces nothing. So no mutation
//! site has to remember to announce itself, whichever call materialised the write, and a
//! plan that has not moved owes nothing however many times this runs.
//!
//! A queued change rides the SAME bounded queue and the same drain the job state changes
//! use ([`super::publish_job_state_changes`]), so this service publishes through one path:
//! the event is sent with no store lock held, in the Region the plan's own ARN names, and
//! publication is best effort — an emulator with no EventBridge mounted has nowhere to
//! publish, so a failure is logged and the operation's own result stands.

use std::collections::BTreeMap;
use std::time::SystemTime;

use super::events::{enqueue, epoch_millis, insert_instant, JobStateChange};
use super::{BackupPlanRecord, ComAmazonawsBackupCryocontrollerusermanagerState};

/// The `detail-type` of the event published when a stored BACKUP PLAN is modified or
/// deleted.
pub const BACKUP_PLAN_STATE_CHANGE: &str = "Backup Plan State Change";

/// The transition a plan state change reports in `detail.state`.
///
/// The two moves a STORED plan makes, which are the ones announced: an update supersedes
/// its current version, and a delete records its deletion. Each carries the instant the
/// stored plan records for it ([`Self::moved_at`]), so the pair cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanTransition {
    /// The plan's body was stored again under the same plan id, as another version.
    Modified,
    /// The plan's deletion was recorded.
    Deleted,
}

impl PlanTransition {
    /// `detail.state` as the event spells this transition.
    fn as_str(self) -> &'static str {
        match self {
            Self::Modified => PLAN_STATE_MODIFIED,
            Self::Deleted => PLAN_STATE_DELETED,
        }
    }

    /// WHEN `plan` made this transition: the instant the stored plan records for it — its
    /// deletion instant for the deletion, and otherwise the instant its current version
    /// was stored, which is the instant the update that made that version was served. Read
    /// off the plan, so the event's `time` is the instant of the mutation rather than the
    /// instant the queue was drained.
    fn moved_at(self, plan: &BackupPlanRecord) -> SystemTime {
        match self {
            Self::Deleted => plan.deletion_date.unwrap_or(plan.current.creation_date),
            Self::Modified => plan.current.creation_date,
        }
    }
}

/// `detail.state` of the event an UPDATE of a plan owes — the mutation that stores
/// another version of the plan's body under the same plan id.
pub const PLAN_STATE_MODIFIED: &str = "MODIFIED";

/// `detail.state` of the event a plan's DELETE owes.
pub const PLAN_STATE_DELETED: &str = "DELETED";

/// What was last published about ONE stored plan: the version the plan was at, and
/// whether its deletion had been announced.
///
/// Those two are what a plan's mutations move: an update replaces the current version, and
/// a delete records the deletion, so a change in either is a transition that owes an event
/// and a stored plan equal to its snapshot owes none.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PublishedBackupPlan {
    /// The `VersionId` of the plan's current version as last published.
    version_id: String,
    /// Whether the plan's deletion has already been published.
    deleted: bool,
}

/// What has already been published about each stored backup plan.
///
/// Held in the store beside the plans, so the diff is taken against what was last
/// PUBLISHED rather than against whatever a caller last read. It does not grow past the
/// plans the store holds: an entry for a plan the store no longer keeps can never differ
/// again and is pruned.
#[derive(Debug, Default)]
pub struct BackupPlanEventLog {
    /// What was last published for each plan, by `BackupPlanId`.
    published: BTreeMap<String, PublishedBackupPlan>,
}

impl ComAmazonawsBackupCryocontrollerusermanagerState {
    /// Queue one event for every stored plan whose mutations differ from what was last
    /// published for it, and take what is stored to be the published state.
    ///
    /// This is the whole of the "which plan mutations owe an event" rule. A plan seen for
    /// the FIRST time only seeds the snapshot: the plan a create stored is what that
    /// call's own response reports, not a move that followed it. A plan whose current
    /// version id has moved on owes a MODIFIED event, and a plan whose deletion is now
    /// recorded owes a DELETED one; a plan that moved both ways since the last record owes
    /// both, oldest transition first.
    ///
    /// Call it under the store lock, after whatever the operation materialised — the
    /// create included, which is what seeds the snapshot the next mutation is compared
    /// against — and then [`super::publish_job_state_changes`] with the lock released.
    pub fn record_backup_plan_state_changes(&mut self) {
        // The plans, the snapshots and the queue are DISJOINT fields of the store, so a
        // stored plan is read while the snapshot and the queue are written.
        let Self {
            backup_plans,
            backup_plan_events,
            job_events,
            ..
        } = self;
        for (plan_id, plan) in backup_plans.iter() {
            let deleted = plan.deletion_date.is_some();
            let published = backup_plan_events.published.get(plan_id).cloned();
            // An update stored another version of the body under the same plan id, which
            // is the one thing that moves the current version id. A plan with no snapshot
            // yet is the create's own, and seeds one below rather than announcing itself.
            if published
                .as_ref()
                .is_some_and(|snapshot| snapshot.version_id != plan.current.version_id)
            {
                enqueue(
                    &mut job_events.pending,
                    plan_change(plan, PlanTransition::Modified),
                );
            }
            // The deletion is a transition of its own, and the last one a plan makes: it
            // is announced once, however many reads of the plan follow it. A plan whose
            // deletion the very call that stored it recorded has no snapshot yet, and that
            // deletion is still a move of the stored plan, so it is announced too.
            if deleted && published.is_none_or(|snapshot| !snapshot.deleted) {
                enqueue(
                    &mut job_events.pending,
                    plan_change(plan, PlanTransition::Deleted),
                );
            }
            backup_plan_events.published.insert(
                plan_id.clone(),
                PublishedBackupPlan {
                    version_id: plan.current.version_id.clone(),
                    deleted,
                },
            );
        }
        // A snapshot belongs to a plan: one the store no longer holds can never differ
        // again, so the snapshots do not grow past the plans they are about.
        backup_plan_events
            .published
            .retain(|plan_id, _| backup_plans.contains_key(plan_id));
    }
}

/// The change one mutation of `plan` owes: the plan's own stored members as the event
/// `detail`, the transition it made as `detail.state`, and the plan ITSELF as the resource
/// the event is about.
///
/// Every other member is the one a read of the same plan reports for it (`GetBackupPlan`,
/// `ListBackupPlans`), under that member's name with a lower-case first letter as an event
/// detail spells it — so the plan a client reacts to on the bus and the plan it then reads
/// back cannot disagree. A member the stored plan does not carry (the deletion instant of
/// a live plan, the last execution of a plan nothing has run) is absent from the detail
/// rather than reported as null, exactly as it is absent from that read.
fn plan_change(plan: &BackupPlanRecord, transition: PlanTransition) -> JobStateChange {
    let mut detail = ::serde_json::Map::new();
    detail.insert("backupPlanId".to_string(), plan.plan_id.clone().into());
    detail.insert("state".to_string(), transition.as_str().into());
    detail.insert("backupPlanArn".to_string(), plan.arn.clone().into());
    detail.insert(
        "backupPlanName".to_string(),
        plan.current.name.clone().into(),
    );
    detail.insert(
        "versionId".to_string(),
        plan.current.version_id.clone().into(),
    );
    insert_instant(&mut detail, "creationDate", Some(plan.current.creation_date));
    insert_instant(&mut detail, "deletionDate", plan.deletion_date);
    insert_instant(&mut detail, "lastExecutionDate", plan.last_execution_date);
    JobStateChange {
        detail_type: BACKUP_PLAN_STATE_CHANGE,
        // A plan belongs to the Region its own ARN names, so that Region's default bus is
        // the one its events arrive on — whichever call or Region drains the queue.
        region: plan_region(&plan.arn),
        resource: plan.arn.clone(),
        at: epoch_millis(transition.moved_at(plan)),
        detail: ::serde_json::Value::Object(detail),
    }
}

/// The Region the backup-plan ARN `plan_arn` names.
///
/// Read off the STORED ARN — `arn:<partition>:backup:<region>:<account>:backup-plan:<id>`,
/// the form [`super::backup_plan_arn`] mints — so the bus a plan's events reach is a
/// property of the plan rather than of whichever call published them. An ARN that names no
/// Region, which nothing this service stores leaves, falls back to the Region an unsigned
/// request is served in ([`super::DEFAULT_REGION`]), so a change is still published
/// somewhere rather than dropped.
fn plan_region(plan_arn: &str) -> String {
    plan_arn
        .split(':')
        .nth(3)
        .filter(|region| !region.is_empty())
        .unwrap_or(super::DEFAULT_REGION)
        .to_string()
}
