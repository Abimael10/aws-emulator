// SPDX-License-Identifier: CC0-1.0

//! The ECS MANAGED DAEMON scheduler: the revisions a daemon's configuration is captured
//! as, the deployments that roll them out, and the one daemon task each container
//! instance of the daemon's capacity providers runs.
//!
//! # The rules this module implements
//!
//! * `CreateDaemon`: "Amazon ECS deploys exactly one daemon task on each container
//!   instance of the specified capacity providers. When a container instance registers
//!   with the cluster, Amazon ECS automatically starts daemon tasks." So the task set a
//!   daemon wants is not a number a request carries: it is ONE task per container
//!   instance that accepts a placement, maintained continuously — which is why
//!   [`ComAmazonawsEcsAmazonec2containerservicev20141113State::maintain_daemon_tasks`] is
//!   run by the clock pass rather than by whichever call happens to arrive. An instance
//!   that joins later gets its daemon task from the instant it joined, and an instance
//!   that stops accepting placements (it is draining, its agent is gone, its registration
//!   was taken back) has the daemon task taken off it.
//! * `DaemonRevision`: "A daemon revision is a snapshot of the daemon's configuration at
//!   the time a deployment was initiated" — so the call that creates or updates a daemon
//!   captures one and never rewrites an earlier one.
//! * `DaemonDeployment`: "A daemon deployment orchestrates the progressive rollout of
//!   daemon task updates across container instances." A rollout is therefore NOT finished
//!   in the call that starts it: it is reported [`DAEMON_DEPLOYMENT_PENDING`] while it has
//!   not started replacing tasks, [`DAEMON_DEPLOYMENT_IN_PROGRESS`] while it does, and
//!   [`DAEMON_DEPLOYMENT_SUCCESSFUL`] once every instance runs the revision it was rolling
//!   out — each step owed by the CLOCK one [`TRANSITION_SPAN`] later, stored as a due
//!   instant and never slept on. A deployment a later one replaces ends in
//!   [`DAEMON_DEPLOYMENT_STOPPED`]: the rollout it was making is not the one being made
//!   any more.
//! * `DeleteDaemon`: the delete is answered with the daemon in
//!   [`DAEMON_DELETE_IN_PROGRESS`] and the daemon's tasks stopped, and the daemon itself is
//!   gone one span later — so `ListDaemons` reflects a create and a delete without anybody
//!   calling in between.
//!
//! Every count a read reports is COUNTED off the tasks and container instances this store
//! holds ([`DaemonTally`]), never bumped by whoever called: a daemon that holds no tasks
//! reports none however many deployments it has had.

use super::*;

/// `DaemonStatus` for a daemon whose delete has been asked for. The other value of that
/// enum beside [`DAEMON_ACTIVE`] (`smithy_query(query="shape", shape="DaemonStatus")`).
pub const DAEMON_DELETE_IN_PROGRESS: &str = "DELETE_IN_PROGRESS";

/// `DaemonDeploymentStatus` for a rollout that has been created and has not begun
/// replacing tasks yet — the status a deployment is answered in, one
/// [`TRANSITION_SPAN`] before it starts.
pub const DAEMON_DEPLOYMENT_PENDING: &str = "PENDING";

/// `DaemonDeploymentStatus` for a rollout that is replacing the daemon's tasks.
pub const DAEMON_DEPLOYMENT_IN_PROGRESS: &str = "IN_PROGRESS";

/// `DaemonDeploymentStatus` for a rollout every container instance of the daemon now runs
/// the target revision of: its terminal, successful end.
pub const DAEMON_DEPLOYMENT_SUCCESSFUL: &str = "SUCCESSFUL";

/// `DaemonDeploymentStatus` for a rollout that ended without reaching its target — the
/// end a deployment a LATER deployment replaced takes, and the end the delete of the
/// daemon leaves an open rollout in.
pub const DAEMON_DEPLOYMENT_STOPPED: &str = "STOPPED";

/// `DaemonNotFoundException$message` for a `daemonArn` that resolves to no stored daemon,
/// as the operations that READ a daemon state it — `DescribeDaemon`, and the listing of a
/// daemon's rollouts under the error it models instead.
///
/// The template AWS words for those calls: the fixed text with the identifier the request
/// carried after it — the only value a client can match the refusal back to, since the
/// daemon it names is not there to report anything else about. The call that DELETES the
/// daemon states the same condition in its own words
/// ([`DAEMON_METADATA_NOT_FOUND_MESSAGE`]): the sentence belongs to the call that was
/// refused, not to the store it looked in.
pub fn daemon_not_found_message(reference: &str) -> String {
    format!("Daemon not found: {}", reference.trim())
}

/// `DaemonNotFoundException$message` for the DELETE of a daemon no stored metadata backs.
///
/// A delete is answered from the daemon's own metadata — the record the create wrote, which
/// is what the delete would move to `DELETE_IN_PROGRESS` — so Amazon ECS states that there
/// is none of it to act on: ONE fixed sentence, naming neither the daemon nor the cluster
/// the request carried.
pub const DAEMON_METADATA_NOT_FOUND_MESSAGE: &str = "No daemon metadata found.";

/// `DaemonNotActiveException$message` for a `daemonArn` this store HOLDS in a status that
/// is not [`DAEMON_ACTIVE`] — the error shape's own doc: "The specified daemon isn't
/// active. You can't update a daemon that's inactive." One fixed sentence, like
/// [`SERVICE_NOT_ACTIVE_MESSAGE`].
pub const DAEMON_NOT_ACTIVE_MESSAGE: &str = "Daemon was not ACTIVE.";

/// The ARN resource type a DAEMON REVISION is named under, as the resource segment of
/// [`daemon_revision_arn`] — its own resource beside the daemon and the deployment that
/// rolls it out.
pub const DAEMON_REVISION_RESOURCE_TYPE: &str = "daemon-revision";

/// The ARN of a daemon revision:
/// `arn:aws:ecs:<region>:<account>:daemon-revision/<cluster>/<daemon>/<id>`.
pub fn daemon_revision_arn(
    region: &str,
    cluster_name: &str,
    daemon_name: &str,
    revision_id: &str,
) -> String {
    ::engine::emulator_arn(
        ARN_NAMESPACE,
        region,
        &format!("{DAEMON_REVISION_RESOURCE_TYPE}/{cluster_name}/{daemon_name}/{revision_id}"),
    )
}

/// How many characters the id a daemon's deployments and revisions are numbered by is
/// spelled in: the fixed width [`ComAmazonawsEcsAmazonec2containerservicev20141113State::next_daemon_deployment_arn`]
/// and [`ComAmazonawsEcsAmazonec2containerservicev20141113State::next_daemon_revision_arn`]
/// render their counter at, so sorting two such ARNs orders what they name oldest first.
///
/// How this service SPELLS that id is its own doing, so it is no part of what an ARN a
/// caller supplies has to look like ([`is_daemon_scoped_arn`]): the id is minted here, and
/// a client cannot be asked to reproduce this service's numbering for its ARN to be read as
/// the identifier it is.
pub const DAEMON_SEQUENCE_ID_WIDTH: usize = 19;

/// The id token one daemon deployment or daemon revision is numbered with, spelled at the
/// one width [`DAEMON_SEQUENCE_ID_WIDTH`] fixes.
pub(super) fn daemon_sequence_id(sequence: u64) -> String {
    format!("{sequence:0width$}", width = DAEMON_SEQUENCE_ID_WIDTH)
}

/// The segments of the RESOURCE PATH `arn` carries when it is an ARN of a `resource_type`
/// resource of THIS service — `arn:<partition>:ecs:<region>:<account>:<type>/<path>` — and
/// `None` for anything else: a value that is no ARN, an ARN of another vendor, one that
/// names no Region or account, and one of another resource type.
///
/// A SYNTAX question, not a lookup, as [`is_service_deployment_arn`] is for the service
/// side: WHICH Region and account the ARN names is part of the resource's identity rather
/// than of its shape (an ARN of another Region names a resource this store does not hold),
/// but an ARN that names neither identifies no resource at all. The ARN is read through the
/// project's ONE parse of a caller-supplied ARN (`service_common::arn`).
fn daemon_scoped_arn_path<'a>(arn: &'a str, resource_type: &str) -> Option<Vec<&'a str>> {
    let arn = arn.trim();
    let parsed = ::service_common::arn::parse(arn)?;
    if parsed.vendor != ARN_NAMESPACE || parsed.region.is_empty() || parsed.account.is_empty() {
        return None;
    }
    let path = ::service_common::arn::resource_id_of_type(arn, resource_type)?;
    Some(path.split('/').collect())
}

/// Whether `arn` is SHAPED like the ARN of a `resource_type` resource BELOW a daemon —
/// `<type>/<cluster>/<daemon>/<id>` ([`daemon_scoped_arn_path`]), with names a cluster and a
/// daemon may carry ([`is_valid_name`]) and an id in the last segment.
///
/// What is asked is which RESOURCE the value identifies, so it is asked of the ARN's shape:
/// the resource type, the cluster and the daemon the resource sits under, and an id. How
/// that id is SPELLED is no part of it — the service mints it
/// ([`DAEMON_SEQUENCE_ID_WIDTH`]) — so an ARN of this shape whose id this store holds
/// nothing under still names a resource of this account's daemon, and a read reports it as
/// the MISSING resource it is rather than refusing the request that carried it.
fn is_daemon_scoped_arn(arn: &str, resource_type: &str) -> bool {
    daemon_scoped_arn_path(arn, resource_type).is_some_and(|segments| {
        matches!(
            segments.as_slice(),
            [cluster_name, daemon_name, id]
                if is_valid_name(cluster_name)
                    && is_valid_name(daemon_name)
                    && !id.is_empty()
        )
    })
}

/// Whether `arn` is shaped like a DAEMON DEPLOYMENT's ARN
/// (`daemon-deployment/<cluster>/<daemon>/<id>`, [`is_daemon_scoped_arn`]).
pub fn is_daemon_deployment_arn(arn: &str) -> bool {
    is_daemon_scoped_arn(arn, DAEMON_DEPLOYMENT_RESOURCE_TYPE)
}

/// Whether `arn` is shaped like a DAEMON REVISION's ARN
/// (`daemon-revision/<cluster>/<daemon>/<id>`, [`is_daemon_scoped_arn`]).
pub fn is_daemon_revision_arn(arn: &str) -> bool {
    is_daemon_scoped_arn(arn, DAEMON_REVISION_RESOURCE_TYPE)
}

/// Whether `arn` is shaped like a DAEMON's own ARN — `daemon/<cluster>/<name>`, with names
/// a cluster and a daemon may carry.
///
/// The same syntax question one resource up: a value of this shape identifies a daemon of
/// some account, whether or not THIS store holds the cluster or the daemon it names.
pub fn is_daemon_arn(arn: &str) -> bool {
    daemon_scoped_arn_path(arn, DAEMON_RESOURCE_TYPE).is_some_and(|segments| {
        matches!(
            segments.as_slice(),
            [cluster_name, daemon_name]
                if is_valid_name(cluster_name) && is_valid_name(daemon_name)
        )
    })
}

/// `InvalidParameterException$message` for a `daemonDeploymentArns` entry that is not
/// shaped like a daemon deployment's ARN ([`is_daemon_deployment_arn`]): the whole sentence
/// is fixed text, stating that the request's parameters are not usable and echoing none of
/// the values it carried.
pub const DAEMON_DEPLOYMENT_ARNS_INVALID_MESSAGE: &str = "Request parameter(s) are invalid.";

/// `InvalidParameterException$message` for a `daemonRevisionArns` entry that is not shaped
/// like a daemon revision's ARN ([`is_daemon_revision_arn`]): fixed text naming the
/// parameter whose syntax is wrong, as [`invalid_service_deployment_arn_message`] does one
/// resource along.
pub const DAEMON_REVISION_ARN_INVALID_MESSAGE: &str = "The Daemon Revision ARN is invalid.";

/// How many deployments this emulator keeps for ONE daemon, and how many revisions with
/// them. EMULATOR-CHOSEN, for the same reason as
/// [`MAX_SERVICE_DEPLOYMENTS_PER_SERVICE`]: every `UpdateDaemon` appends one, so without
/// a bound a caller could grow the in-memory store without limit. The newest are kept.
pub const MAX_DAEMON_DEPLOYMENTS_PER_DAEMON: usize = MAX_SERVICE_DEPLOYMENTS_PER_SERVICE;

/// `Task$group` of a daemon's task: the daemon's own name under the resource that placed
/// it, the way a service's task carries `service:<name>`.
pub fn daemon_task_group(daemon_name: &str) -> String {
    format!("daemon:{daemon_name}")
}

/// `Task$stoppedReason` for a daemon task the rollout of a NEW daemon revision replaces.
/// The daemon scheduler's own account of a stop it made, naming nothing of any request.
pub const DAEMON_TASK_REPLACED_REASON: &str =
    "Daemon deployment initiated a replacement of this task.";

/// `Task$stoppedReason` for a daemon task whose container instance no longer accepts a
/// placement — it is draining, its agent is gone, or its registration was taken back.
pub const DAEMON_TASK_HOST_GONE_REASON: &str =
    "The container instance running this daemon task is no longer available.";

/// `Task$stoppedReason` for a daemon task stopped because the daemon it belongs to is
/// being deleted.
pub const DAEMON_TASK_DELETED_REASON: &str = "The daemon running this task was deleted.";

/// `DaemonDeployment$statusReason` of a rollout a LATER rollout of the same daemon
/// replaced: the deployment that replaced it is what the sentence names, because that is
/// the one a client has to look at next.
pub fn daemon_deployment_replaced_reason(deployment_arn: &str) -> String {
    format!("Daemon deployment {deployment_arn} replaced this deployment.")
}

/// `DaemonDeployment$statusReason` of a rollout that reached its target revision on every
/// container instance.
pub const DAEMON_DEPLOYMENT_COMPLETED_REASON: &str = "Daemon deployment completed.";

/// `DaemonDeployment$statusReason` of the rollout the DELETE of its daemon ended.
pub const DAEMON_DEPLOYMENT_DELETED_REASON: &str =
    "The daemon this deployment belongs to was deleted.";

/// `DaemonDeployment$statusReason` of the deployment a DELETE starts: "This deployment
/// drains existing daemon tasks from the container instances"
/// (`DeleteDaemonResponse$deploymentArn`).
pub const DAEMON_DEPLOYMENT_DRAINING_REASON: &str =
    "Daemon deployment is draining the daemon tasks of the deleted daemon.";

/// The page `ListDaemonDeployments` reports when the request names no `maxResults`. The
/// member's doc: "If this parameter isn't used, then `ListDaemonDeployments` returns up to
/// 20 results and a `nextToken` value if applicable."
pub const DEFAULT_LIST_DAEMON_DEPLOYMENTS_PAGE_SIZE: usize = 20;

/// The most daemon deployments one `DescribeDaemonDeployments` may ask about. The
/// `daemonDeploymentArns` doc: "You can specify up to 20 ARNs."
pub const MAX_DESCRIBE_DAEMON_DEPLOYMENTS: usize = 20;

/// The most daemon revisions one `DescribeDaemonRevisions` may ask about. The
/// `daemonRevisionArns` doc: "You can specify up to 20 ARNs."
pub const MAX_DESCRIBE_DAEMON_REVISIONS: usize = 20;

/// Why a `capacityProviderArns` entry names no capacity provider a daemon can deploy onto
/// — the two conditions
/// [`ComAmazonawsEcsAmazonec2containerservicev20141113State::resolve_daemon_capacity_providers`]
/// tells apart. Shared by every call that takes that list, so a create and an update refuse
/// the same list for the same reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonCapacityRefusal {
    /// The entry names no provider this account holds.
    NotFound,
    /// It names one whose `CapacityProvider$type` is not
    /// [`CAPACITY_PROVIDER_TYPE_MANAGED_INSTANCES`]: "ECS Managed Daemons is only supported
    /// for Amazon ECS Managed Instances Capacity Providers."
    NotManagedInstances,
}

/// Why a `daemonTaskDefinitionArn` names no daemon revision a daemon can be put on — the
/// conditions
/// [`ComAmazonawsEcsAmazonec2containerservicev20141113State::resolve_daemon_task_definition`]
/// tells apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonDefinitionRefusal {
    /// The family part of the reference is not a name of the documented character set, so
    /// the sentence states the pattern a family has to match.
    FamilyPattern(&'static str),
    /// The value is not a daemon task-definition reference at all.
    Malformed(String),
    /// It names no stored revision.
    NotFound,
    /// It names a revision this store HOLDS in a status that can run no new work.
    Inactive,
}

/// One DAEMON REVISION: the daemon's configuration as the call that started a rollout of
/// it captured it.
///
/// `DaemonRevision`: "A daemon revision is a snapshot of the daemon's configuration at the
/// time a deployment was initiated" — so this record is written once and never changed: a
/// later update captures another revision rather than rewriting this one.
#[derive(Debug, Clone)]
pub struct DaemonRevisionRecord {
    pub arn: String,
    pub daemon_arn: String,
    pub cluster_arn: String,
    /// The daemon task-definition revision this daemon revision runs, as its ARN.
    pub daemon_task_definition_arn: String,
    /// The capacity providers the daemon deployed onto when this revision was captured,
    /// by NAME — the key `capacity_providers` holds them under.
    pub capacity_provider_names: Vec<String>,
    /// `DaemonRevision$containerImages`: "The container images used by the daemon
    /// revision", read off the daemon task definition the revision runs.
    pub container_images: Vec<DaemonContainerImage>,
    pub propagate_tags: String,
    pub enable_ecs_managed_tags: bool,
    pub enable_execute_command: bool,
    pub critical: bool,
    pub created_at: DateTime,
}

impl DaemonRevisionRecord {
    /// The `DaemonRevision` `DescribeDaemonRevisions` reports for this revision, built
    /// from the snapshot it stored.
    pub fn view(&self) -> DaemonRevision {
        DaemonRevision {
            daemon_revision_arn: Some(self.arn.clone()),
            cluster_arn: Some(self.cluster_arn.clone()),
            daemon_arn: Some(self.daemon_arn.clone()),
            daemon_task_definition_arn: Some(self.daemon_task_definition_arn.clone()),
            created_at: Some(self.created_at),
            container_images: Some(self.container_images.clone()),
            propagate_tags: Some(self.propagate_tags.clone()),
            enable_ecs_managed_tags: Some(self.enable_ecs_managed_tags),
            enable_execute_command: Some(self.enable_execute_command),
            critical: Some(self.critical),
        }
    }
}

/// One DAEMON DEPLOYMENT: the rollout of one daemon revision across the container
/// instances of the daemon's capacity providers.
#[derive(Debug, Clone)]
pub struct DaemonDeploymentRecord {
    pub arn: String,
    pub daemon_arn: String,
    pub cluster_arn: String,
    /// One of `DaemonDeploymentStatus`; see [`DAEMON_DEPLOYMENT_PENDING`].
    pub status: String,
    /// `DaemonDeployment$statusReason`: "Information about why the daemon deployment is in
    /// the current status", and `None` while it is simply under way.
    pub status_reason: Option<String>,
    /// The revision this rollout is deploying, and `None` for the DRAIN a delete starts:
    /// there the daemon is going and no revision is being deployed in its place.
    pub target_revision_arn: Option<String>,
    /// The revisions it is replacing — the ones deployed when it started, newest first.
    pub source_revision_arns: Vec<String>,
    /// The deployment configuration this rollout runs under, as the call that started it
    /// carried it. `None` when that call named none, which is what the member being
    /// optional means.
    pub deployment_configuration: Option<DaemonDeploymentConfiguration>,
    pub created_at: DateTime,
    /// `DaemonDeployment$startedAt`: "The time the daemon deployment started" — the
    /// instant it began replacing tasks, so `None` for one that has not.
    pub started_at: Option<DateTime>,
    /// `DaemonDeployment$stoppedAt`: "The time the daemon deployment stopped", carried by
    /// exactly the rollouts that ended without reaching their target.
    pub stopped_at: Option<DateTime>,
    /// `DaemonDeployment$finishedAt`: "The time the daemon deployment finished" — set for
    /// every rollout that has ended, however it ended.
    pub finished_at: Option<DateTime>,
    /// When this rollout STARTS replacing tasks, and `None` once it has.
    pub started_due_at: Option<SystemTime>,
    /// When it reaches its end, and `None` until it has started or once it has ended.
    pub finished_due_at: Option<SystemTime>,
}

impl DaemonDeploymentRecord {
    /// Whether this rollout has ended — exactly when it carries a `finishedAt`, so a
    /// reported status and a reported finish time can never disagree.
    pub fn is_finished(&self) -> bool {
        self.finished_at.is_some()
    }

    /// The key a listing orders deployments by: the `createdAt` it reports, with the ARN
    /// breaking a tie between two created in the same instant (its last segment is minted
    /// from a monotonic fixed-width counter, so the later deployment always sorts after
    /// the earlier one).
    pub fn order_key(&self) -> ((i64, u32), &str) {
        (timestamp_key(self.created_at), self.arn.as_str())
    }

    /// The `DaemonDeploymentSummary` `ListDaemonDeployments` reports for this rollout.
    pub fn summary(&self) -> DaemonDeploymentSummary {
        DaemonDeploymentSummary {
            daemon_deployment_arn: Some(self.arn.clone()),
            daemon_arn: Some(self.daemon_arn.clone()),
            cluster_arn: Some(self.cluster_arn.clone()),
            status: Some(self.status.clone()),
            status_reason: self.status_reason.clone(),
            target_daemon_revision_arn: self.target_revision_arn.clone(),
            created_at: Some(self.created_at),
            started_at: self.started_at,
            stopped_at: self.stopped_at,
            finished_at: self.finished_at,
        }
    }

    /// The CloudWatch alarms this rollout watches, as its own deployment configuration
    /// turned them on.
    ///
    /// `DaemonDeploymentAlarms$status`: "Amazon ECS is not using alarms for daemon
    /// deployment failures when the status is `DISABLED`" — which is every rollout whose
    /// configuration enabled none. A rollout that enabled them is MONITORING while it runs
    /// and its watch is complete once it has ended; no alarm is ever reported as triggered,
    /// because nothing here evaluates a CloudWatch alarm.
    fn alarms(&self) -> DaemonDeploymentAlarms {
        let configured = self
            .deployment_configuration
            .as_ref()
            .and_then(|configuration| configuration.alarms.as_ref());
        let enabled = configured.is_some_and(|alarms| alarms.enable);
        let status = match (enabled, self.is_finished()) {
            (false, _) => ROLLBACK_MONITORS_DISABLED,
            (true, false) => ROLLBACK_MONITORS_MONITORING,
            (true, true) => ROLLBACK_MONITORS_MONITORING_COMPLETE,
        };
        DaemonDeploymentAlarms {
            status: Some(status.to_string()),
            alarm_names: Some(
                configured
                    .and_then(|alarms| alarms.alarm_names.clone())
                    .unwrap_or_default(),
            ),
            triggered_alarm_names: Some(Vec::new()),
        }
    }
}

/// The Region a daemon's own ARNs carry, read off the cluster that hosts it.
///
/// The one reading of it in this module: a task or a revision a daemon's scheduler mints
/// names the Region the DAEMON lives in rather than whichever Region last called, and a
/// clock pass has no request to take one from.
fn daemon_region(daemon: &DaemonRecord) -> String {
    super::request_arn::RequestArn::parse(&daemon.cluster_arn)
        .map(|arn| arn.region.to_string())
        .unwrap_or_else(|| DEFAULT_REGION.to_string())
}

/// What ONE revision's tasks and their hosts add up to, counted off the task and
/// container-instance maps: the instances running the revision's daemon task, the ones
/// running none, and the ones being drained — per capacity provider and in total.
///
/// Counted rather than stored, so a reported count cannot drift from the tasks that exist.
///
/// An instance whose daemon task has been PLACED and has not started yet is in none of the
/// three: `runningInstanceCount` is "the number of instances running daemon tasks", and
/// `withoutDaemonInstanceCount` the ones "running without the daemon task. This applies to
/// daemons that aren't critical, where the instance remains available for your other tasks
/// even if the daemon task can't start or stops" — a task on its way up has neither started
/// nor failed to, so the counts of a rollout in flight need not add up to the cluster's
/// instances while a task of it is still starting.
#[derive(Debug, Clone, Default)]
pub struct DaemonTally {
    /// Per capacity provider, in the order the daemon revision names them:
    /// `(provider name, running, without daemon, draining)`.
    pub by_provider: Vec<(String, i32, i32, i32)>,
    pub running: i32,
    pub without_daemon: i32,
    pub draining: i32,
}

/// Where one container instance stands as far as ONE daemon revision is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostStanding {
    /// It is being drained, so the daemon task on it is on its way off.
    Draining,
    /// It runs the revision's daemon task.
    Running,
    /// It runs no daemon task of this daemon at all.
    WithoutDaemon,
    /// It has one placed and not started yet, so it is neither running the daemon nor
    /// without it.
    Placing,
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// The CLUSTER and the daemon NAME a `daemonArn` refers to, or `None` when the value is
    /// not a daemon ARN at all.
    ///
    /// Every daemon member takes "The Amazon Resource Name (ARN) of the daemon", and a
    /// daemon ARN carries the cluster that hosts it (`daemon/<cluster>/<name>`), so the one
    /// value names both. The ARN is read through the project's ONE parse of a
    /// caller-supplied ARN (`service_common::arn`), the same reading every other
    /// cluster-scoped resource of this service gets.
    pub fn daemon_arn_parts(reference: &str) -> Option<(String, String)> {
        let resource =
            ::service_common::arn::resource_id_of_type(reference.trim(), DAEMON_RESOURCE_TYPE)?;
        let (cluster_name, daemon_name) = resource.split_once('/')?;
        (!cluster_name.is_empty() && !daemon_name.is_empty())
            .then(|| (cluster_name.to_string(), daemon_name.to_string()))
    }

    /// The CLUSTER a `daemonArn` carries that this account does not hold as an active
    /// cluster, or `None` when the ARN carries one it does (or carries none at all).
    ///
    /// For the operations that ACT in the daemon's cluster rather than merely reporting the
    /// daemon the ARN identifies: a daemon's cluster is where its tasks run, so a call that
    /// changes what runs there resolves that cluster first and is refused about the cluster
    /// when it is not there — `ClusterNotFoundException`, the refusal every cluster-scoped
    /// operation of this service answers ([`cluster_refusal_message`]). A pure read of one
    /// daemon takes the ARN as the whole identifier instead ([`Self::daemon_from_arn`]).
    pub fn daemon_arn_cluster_refusal(&self, reference: &str) -> Option<String> {
        let (cluster_name, _) = Self::daemon_arn_parts(reference)?;
        (!self.has_active_cluster(&cluster_name)).then_some(cluster_name)
    }

    /// Whether a reference of `resource_type` names a resource BELOW a daemon this account
    /// holds no record of — a cluster it never created, or a daemon that cluster does not
    /// hold.
    ///
    /// A `daemon-deployment`/`daemon-revision` ARN carries the daemon the resource belongs to
    /// (`<type>/<cluster>/<daemon>/<id>`, [`is_daemon_scoped_arn`]), and those resources exist
    /// only as part of a daemon's own history: when the daemon is not there, no id under it
    /// can name a rollout or a revision of this account, so such an entry is the unusable
    /// parameter it is rather than one resource reported as `MISSING`. Within a daemon this
    /// account DOES hold, an id this store holds nothing under is exactly that `MISSING`
    /// report, whatever the id looks like.
    ///
    /// `false` when `references` names no such daemon at all (every entry is not an ARN of
    /// `resource_type`): what the entries ARE is settled before this, by the shape test.
    pub fn names_an_unheld_daemon<'a>(
        &self,
        references: impl IntoIterator<Item = &'a str>,
        resource_type: &str,
    ) -> bool {
        references.into_iter().any(|reference| {
            daemon_scoped_arn_path(reference.trim(), resource_type).is_some_and(|segments| {
                matches!(
                    segments.as_slice(),
                    [cluster_name, daemon_name, _] if !self.holds_daemon(cluster_name, daemon_name)
                )
            })
        })
    }

    /// Whether this account holds the daemon `daemon_name` of the cluster `cluster_name` —
    /// the pair a daemon ARN identifies a daemon by ("Amazon ECS daemons are cluster
    /// specific"), read off the stored records rather than off an ARN, so an entry naming the
    /// daemon in another Region's or account's spelling resolves to the same daemon.
    fn holds_daemon(&self, cluster_name: &str, daemon_name: &str) -> bool {
        self.daemons
            .values()
            .any(|daemon| daemon.cluster_name == cluster_name && daemon.name == daemon_name)
    }

    /// The stored daemon a `daemonArn` names, or `None` when no daemon of this account
    /// answers to it.
    ///
    /// The ARN identifies the daemon by itself — it carries the cluster that hosts it and
    /// the daemon's own name — so the lookup is that one identifier, and anything it does
    /// not resolve is a daemon that is not there: an ARN under a cluster this account never
    /// created names no daemon just as a name no daemon of an existing cluster answers to
    /// does ("Amazon ECS daemons are cluster specific and Region specific"), and a value
    /// that is no daemon ARN at all names none either. Every operation addressed to ONE
    /// daemon refuses all three the same way, in the sentence that call words for the
    /// condition ([`daemon_not_found_message`] for a read,
    /// [`DAEMON_METADATA_NOT_FOUND_MESSAGE`] for the delete).
    pub fn daemon_from_arn(&self, reference: &str) -> Option<&DaemonRecord> {
        let reference = reference.trim();
        Self::daemon_arn_parts(reference)?;
        self.daemons.values().find(|daemon| daemon.arn == reference)
    }

    /// Every capacity provider a daemon's `capacityProviderArns` names, by NAME, in the
    /// order the request named them — or WHY one of them names no provider a daemon can
    /// deploy onto.
    ///
    /// "ECS Managed Daemons is only supported for Amazon ECS Managed Instances Capacity
    /// Providers": every entry has to resolve to a provider of the account whose
    /// `CapacityProvider$type` is [`CAPACITY_PROVIDER_TYPE_MANAGED_INSTANCES`], so the FIRST
    /// entry that does not refuses the whole call and nothing is stored. The one resolution
    /// both the create and the update of a daemon make, so neither accepts a list the other
    /// would refuse.
    pub fn resolve_daemon_capacity_providers(
        &self,
        references: &[String],
        region: &str,
    ) -> Result<Vec<String>, DaemonCapacityRefusal> {
        let mut names = Vec::with_capacity(references.len());
        for reference in references {
            let name = capacity_provider_name_from_reference(reference);
            let provider = self
                .capacity_provider(&name, region)
                .ok_or(DaemonCapacityRefusal::NotFound)?;
            if provider.provider_type.as_deref() != Some(CAPACITY_PROVIDER_TYPE_MANAGED_INSTANCES) {
                return Err(DaemonCapacityRefusal::NotManagedInstances);
            }
            names.push(provider.name);
        }
        Ok(names)
    }

    /// The daemon task-definition REVISION a `daemonTaskDefinitionArn` names, as its ARN —
    /// or WHY it names none.
    ///
    /// The revision is resolved ONCE, by the call that points a daemon at it, so a
    /// registration made later in the same family does not move the daemon onto it; a
    /// reference that names no revision takes the latest ACTIVE one of its family, as the
    /// family alone does everywhere this resource is read. The one resolution both the
    /// create and the update of a daemon make.
    pub fn resolve_daemon_task_definition(
        &self,
        reference: &str,
    ) -> Result<String, DaemonDefinitionRefusal> {
        if let Some(message) = daemon_task_definition_family_violation(reference) {
            return Err(DaemonDefinitionRefusal::FamilyPattern(message));
        }
        let Some((family, revision)) = parse_daemon_task_definition_reference(reference) else {
            return Err(DaemonDefinitionRefusal::Malformed(
                daemon_task_definition_reference_message(reference),
            ));
        };
        let resolved = match revision {
            Some(revision) => self.daemon_task_definitions.get(&(family, revision)),
            None => self.latest_active_daemon_revision(&family),
        };
        let resolved = resolved.ok_or(DaemonDefinitionRefusal::NotFound)?;
        // A revision whose delete has been requested or carried out is still readable but
        // can run no new work, so it names an existing resource in a status this call
        // cannot accept.
        if resolved.status != DAEMON_TASK_DEFINITION_ACTIVE {
            return Err(DaemonDefinitionRefusal::Inactive);
        }
        Ok(resolved.arn.clone())
    }

    /// Mint the ARN of a daemon's next REVISION. The counter is what makes every revision
    /// of every daemon distinct, and it is fixed-width, so sorting two of these ARNs
    /// orders the revisions they name oldest first.
    pub fn next_daemon_revision_arn(
        &mut self,
        region: &str,
        cluster_name: &str,
        daemon_name: &str,
    ) -> String {
        self.next_daemon_revision_seq += 1;
        daemon_revision_arn(
            region,
            cluster_name,
            daemon_name,
            &daemon_sequence_id(self.next_daemon_revision_seq),
        )
    }

    /// Capture the daemon `daemon_arn` holds as a new DAEMON REVISION and start the DAEMON
    /// DEPLOYMENT that rolls it out, as of `now`; answer the two ARNs it minted.
    ///
    /// Called by `CreateDaemon` and by `UpdateDaemon` AFTER the daemon record carries the
    /// configuration being deployed — that record is what the revision is captured from,
    /// so the revision a read reports and the daemon it belongs to cannot drift.
    ///
    /// A rollout this one replaces never reaches the revision it was rolling out, so it
    /// ends here ([`DAEMON_DEPLOYMENT_STOPPED`]) rather than staying open for an end that
    /// can no longer arrive.
    pub fn start_daemon_deployment(
        &mut self,
        daemon_arn: &str,
        region: &str,
        now: SystemTime,
    ) -> Option<(String, String)> {
        let daemon = self.daemons.get(daemon_arn)?.clone();
        let at = DateTime::from(now);
        let revision_arn =
            self.next_daemon_revision_arn(region, &daemon.cluster_name, &daemon.name);
        let revision = DaemonRevisionRecord {
            arn: revision_arn.clone(),
            daemon_arn: daemon.arn.clone(),
            cluster_arn: daemon.cluster_arn.clone(),
            daemon_task_definition_arn: daemon.daemon_task_definition_arn.clone(),
            capacity_provider_names: daemon.capacity_provider_names.clone(),
            container_images: self.daemon_revision_container_images(&daemon),
            propagate_tags: daemon.propagate_tags.clone(),
            enable_ecs_managed_tags: daemon.enable_ecs_managed_tags,
            enable_execute_command: daemon.enable_execute_command,
            critical: daemon.critical,
            created_at: at,
        };
        self.daemon_revisions
            .insert(revision_arn.clone(), revision);

        // The revisions this rollout is replacing: the ones deployed when it started, which
        // is the revision the daemon was running up to this call.
        let source_revision_arns: Vec<String> = daemon
            .current_revision_arn
            .clone()
            .filter(|arn| *arn != revision_arn)
            .into_iter()
            .collect();
        let deployment_arn =
            self.next_daemon_deployment_arn(region, &daemon.cluster_name, &daemon.name);
        self.stop_open_daemon_deployments(
            &daemon.arn,
            at,
            &daemon_deployment_replaced_reason(&deployment_arn),
        );
        let deployment = DaemonDeploymentRecord {
            arn: deployment_arn.clone(),
            daemon_arn: daemon.arn.clone(),
            cluster_arn: daemon.cluster_arn.clone(),
            // "A daemon deployment orchestrates the progressive rollout of daemon task
            // updates across container instances": nothing has been replaced at the instant
            // the call answers, so the rollout is still PENDING and its start is owed by
            // the clock.
            status: DAEMON_DEPLOYMENT_PENDING.to_string(),
            status_reason: None,
            target_revision_arn: Some(revision_arn.clone()),
            source_revision_arns,
            deployment_configuration: daemon.deployment_configuration.clone(),
            created_at: at,
            started_at: None,
            stopped_at: None,
            finished_at: None,
            started_due_at: Some(now + TRANSITION_SPAN),
            finished_due_at: None,
        };
        self.daemon_deployments
            .insert(deployment_arn.clone(), deployment);
        if let Some(daemon) = self.daemons.get_mut(daemon_arn) {
            daemon.deployment_arn = deployment_arn.clone();
            daemon.current_revision_arn = Some(revision_arn.clone());
            daemon.updated_at = at;
        }
        self.prune_daemon_deployments(daemon_arn);
        Some((deployment_arn, revision_arn))
    }

    /// The container images a revision of `daemon` runs, read off the daemon task
    /// definition it was captured over.
    ///
    /// `DaemonContainerImage` carries the container's name and the image it runs; the
    /// `imageDigest` stays absent, because a digest is what the agent reports once it has
    /// pulled the image and nothing has pulled one here.
    fn daemon_revision_container_images(&self, daemon: &DaemonRecord) -> Vec<DaemonContainerImage> {
        self.daemon_task_definitions
            .values()
            .find(|definition| definition.arn == daemon.daemon_task_definition_arn)
            .map(|definition| {
                definition
                    .container_definitions
                    .iter()
                    .map(|container| DaemonContainerImage {
                        container_name: container.name.clone(),
                        image_digest: None,
                        image: Some(container.image.clone()),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// End every rollout of `daemon_arn` that has not finished yet, at `at`, with `reason`.
    ///
    /// The two things that end one early are a deployment that REPLACES it and the DELETE
    /// of the daemon it was rolling out; both are calls, so neither is a transition the
    /// clock owes.
    fn stop_open_daemon_deployments(&mut self, daemon_arn: &str, at: DateTime, reason: &str) {
        for deployment in self.daemon_deployments.values_mut() {
            if deployment.daemon_arn != daemon_arn || deployment.is_finished() {
                continue;
            }
            deployment.status = DAEMON_DEPLOYMENT_STOPPED.to_string();
            deployment.status_reason = Some(reason.to_string());
            deployment.stopped_at = Some(at);
            deployment.finished_at = Some(at);
            deployment.started_due_at = None;
            deployment.finished_due_at = None;
        }
    }

    /// Drop the oldest of `daemon_arn`'s deployments down to
    /// [`MAX_DAEMON_DEPLOYMENTS_PER_DAEMON`], and with them the revisions nothing names
    /// any more — the way a service's deployments are kept
    /// ([`Self::prune_service_deployments`]).
    fn prune_daemon_deployments(&mut self, daemon_arn: &str) {
        let mut arns: Vec<String> = self
            .daemon_deployments
            .values()
            .filter(|deployment| deployment.daemon_arn == daemon_arn)
            .map(|deployment| deployment.arn.clone())
            .collect();
        if arns.len() > MAX_DAEMON_DEPLOYMENTS_PER_DAEMON {
            // The ARN ends with a fixed-width monotonic id, so sorting orders them oldest
            // first and the ones to drop are the leading entries.
            arns.sort();
            let first_kept = arns.len() - MAX_DAEMON_DEPLOYMENTS_PER_DAEMON;
            for arn in &arns[..first_kept] {
                self.daemon_deployments.remove(arn);
            }
        }
        self.prune_daemon_revisions();
    }

    /// Drop every daemon revision no deployment and no daemon names any more: a revision
    /// is read back through the rollout that released it, so one whose rollout is gone is
    /// unreachable.
    fn prune_daemon_revisions(&mut self) {
        let mut named: HashSet<String> = self
            .daemon_deployments
            .values()
            .flat_map(|deployment| {
                deployment
                    .target_revision_arn
                    .clone()
                    .into_iter()
                    .chain(deployment.source_revision_arns.iter().cloned())
            })
            .collect();
        named.extend(
            self.daemons
                .values()
                .filter_map(|daemon| daemon.current_revision_arn.clone()),
        );
        // A revision a task still runs is named by that task, whatever became of the
        // rollout that released it.
        named.extend(
            self.tasks
                .values()
                .filter_map(|task| task.daemon_revision_arn.clone()),
        );
        self.daemon_revisions.retain(|arn, _| named.contains(arn));
    }

    /// Record the DELETE of the daemon `daemon_arn` names, as of `now`, and answer the ARN
    /// of the daemon deployment that DRAINS its tasks.
    ///
    /// `DaemonStatus` carries [`DAEMON_DELETE_IN_PROGRESS`] for exactly this: the delete is
    /// answered while it is being carried out, so the daemon is reported in that status
    /// with its tasks draining, and the record itself is gone one [`TRANSITION_SPAN`] later
    /// — stored as a due instant and materialised by the clock
    /// ([`Self::advance_daemon_deployments`]), never slept on.
    ///
    /// `DeleteDaemonResponse$deploymentArn` is "the daemon deployment that was triggered by
    /// the delete operation. This deployment drains existing daemon tasks from the container
    /// instances", so the delete starts one: it is under way from this instant, because the
    /// tasks it drains are asked to stop here.
    ///
    /// A delete already under way is not restarted: the second request asks for what the
    /// first one already got, so it answers the drain the first one started.
    pub fn request_daemon_delete(&mut self, daemon_arn: &str, now: SystemTime) -> Option<String> {
        let at = DateTime::from(now);
        let daemon = self.daemons.get(daemon_arn)?.clone();
        if daemon.removal_due_at.is_some() {
            return Some(daemon.deployment_arn.clone());
        }
        let region = daemon_region(&daemon);
        let deployment_arn =
            self.next_daemon_deployment_arn(&region, &daemon.cluster_name, &daemon.name);
        self.stop_open_daemon_deployments(daemon_arn, at, DAEMON_DEPLOYMENT_DELETED_REASON);
        let drain = DaemonDeploymentRecord {
            arn: deployment_arn.clone(),
            daemon_arn: daemon.arn.clone(),
            cluster_arn: daemon.cluster_arn.clone(),
            // The drain is under way from this instant: the tasks it takes off the
            // container instances are asked to stop by this very call.
            status: DAEMON_DEPLOYMENT_IN_PROGRESS.to_string(),
            status_reason: Some(DAEMON_DEPLOYMENT_DRAINING_REASON.to_string()),
            // No revision is being DEPLOYED by a drain: the daemon itself is going, and the
            // revisions whose tasks it takes off the instances are the ones it is replacing.
            target_revision_arn: None,
            source_revision_arns: daemon.current_revision_arn.clone().into_iter().collect(),
            deployment_configuration: daemon.deployment_configuration.clone(),
            created_at: at,
            started_at: Some(at),
            stopped_at: None,
            finished_at: None,
            started_due_at: None,
            // The drain ends with the daemon: the record is gone at the instant the delete
            // is carried out, so nothing is owed on it afterwards.
            finished_due_at: None,
        };
        self.daemon_deployments
            .insert(deployment_arn.clone(), drain);
        if let Some(daemon) = self.daemons.get_mut(daemon_arn) {
            daemon.status = DAEMON_DELETE_IN_PROGRESS.to_string();
            daemon.updated_at = at;
            daemon.removal_due_at = Some(now + TRANSITION_SPAN);
            daemon.deployment_arn = deployment_arn.clone();
        }
        let tasks = self.daemon_task_arns(daemon_arn);
        for task_arn in tasks {
            self.stop_service_task(&task_arn, now, DAEMON_TASK_DELETED_REASON);
        }
        Some(deployment_arn)
    }

    /// The live tasks of the daemon `daemon_arn` names: the ones it still wants running,
    /// so a task already on its way out is not one of them.
    fn daemon_task_arns(&self, daemon_arn: &str) -> Vec<String> {
        self.tasks
            .values()
            .filter(|task| {
                task.daemon_arn.as_deref() == Some(daemon_arn)
                    && task.desired_status == STATUS_RUNNING
                    && task.is_active()
            })
            .map(|task| task.task_arn.clone())
            .collect()
    }

    /// Materialise every DAEMON transition the clock has already made due, as of `now`:
    /// the rollout that starts replacing tasks, the rollout that reaches its end, and the
    /// daemon whose delete has been carried out.
    ///
    /// Each was owed at this instant whether or not anybody called, and each happens at
    /// most once, because the instant it falls due is cleared with it.
    pub(super) fn advance_daemon_deployments(&mut self, now: SystemTime) {
        let arns: Vec<String> = self.daemon_deployments.keys().cloned().collect();
        for arn in arns {
            let Some(deployment) = self.daemon_deployments.get(&arn) else {
                continue;
            };
            // The rollout begins replacing tasks at the instant it was owed at, not at the
            // instant this pass ran.
            if let Some(due) = deployment.started_due_at.filter(|due| *due <= now) {
                if let Some(deployment) = self.daemon_deployments.get_mut(&arn) {
                    deployment.started_due_at = None;
                    deployment.status = DAEMON_DEPLOYMENT_IN_PROGRESS.to_string();
                    deployment.started_at = Some(DateTime::from(due));
                    // The end of a rollout falls due one span after its start, so a caller
                    // reading in between still sees it in progress.
                    deployment.finished_due_at = Some(due + TRANSITION_SPAN);
                }
            }
            let Some(deployment) = self.daemon_deployments.get(&arn) else {
                continue;
            };
            // A rollout ends once every container instance of the daemon runs the revision
            // it was rolling out and none runs another — which the placement pass below
            // brings about, so the end waits for the task set rather than for the clock
            // alone. Weighed only for a rollout whose end is already DUE, so the cost of
            // this pass is one comparison per stored rollout and the task set is walked
            // only for the one rollout that can end now.
            if let Some(due) = deployment.finished_due_at.filter(|due| *due <= now) {
                if let Some(reached) = self.daemon_rollout_reached_its_target(deployment) {
                    if let Some(deployment) = self.daemon_deployments.get_mut(&arn) {
                        deployment.finished_due_at = None;
                        deployment.status = DAEMON_DEPLOYMENT_SUCCESSFUL.to_string();
                        deployment.status_reason =
                            Some(DAEMON_DEPLOYMENT_COMPLETED_REASON.to_string());
                        // The instant the rollout FINISHED: the instant it was owed at, or
                        // the instant its last task started when that came later — a
                        // rollout cannot be reported as finished before the work it
                        // reports.
                        let owed = DateTime::from(due);
                        deployment.finished_at = Some(if timestamp_key(reached) > timestamp_key(owed)
                        {
                            reached
                        } else {
                            owed
                        });
                    }
                }
            }
        }
        // The delete a `DeleteDaemon` call asked for: once it is due the daemon is gone,
        // whether or not anybody called to look — and with it the rollouts and revisions
        // that were read back through it.
        let removed: Vec<String> = self
            .daemons
            .values()
            .filter(|daemon| daemon.removal_due_at.is_some_and(|due| due <= now))
            .map(|daemon| daemon.arn.clone())
            .collect();
        for daemon_arn in removed {
            self.daemons.remove(&daemon_arn);
            self.daemon_deployments
                .retain(|_, deployment| deployment.daemon_arn != daemon_arn);
            self.prune_daemon_revisions();
        }
    }

    /// WHEN the rollout `deployment` reached its target — every container instance of its
    /// daemon running the revision it was rolling out and none running another — or `None`
    /// when it has not.
    ///
    /// The instant is the LAST of its tasks' own `startedAt`, which is when the rollout
    /// really finished; a rollout with no instance to place a task on reached its target
    /// with nothing to start, so it answers the instant it was created at and the end it
    /// takes is the one the clock owed.
    fn daemon_rollout_reached_its_target(
        &self,
        deployment: &DaemonDeploymentRecord,
    ) -> Option<DateTime> {
        let daemon = self.daemons.get(&deployment.daemon_arn)?;
        let target = deployment.target_revision_arn.as_deref()?;
        if daemon.current_revision_arn.as_deref() != Some(target) {
            return None;
        }
        let placed = self.daemon_tasks_by_host(&daemon.arn);
        let converged = self.daemon_hosts(&daemon.cluster_name).iter().all(|host| {
            placed.get(host.as_str()).is_some_and(|task| {
                task.daemon_revision_arn.as_deref() == Some(target)
                    && task.last_status == STATUS_RUNNING
            })
        }) && placed
            .values()
            .all(|task| task.daemon_revision_arn.as_deref() == Some(target));
        if !converged {
            return None;
        }
        Some(
            placed
                .values()
                .filter_map(|task| task.started_at)
                .max_by_key(|at| timestamp_key(*at))
                .unwrap_or(deployment.created_at),
        )
    }

    /// The container instances of `cluster_name` a daemon task runs on, by ARN, in a
    /// deterministic order (the instance ARN's own, whose last segment is minted from a
    /// monotonic counter, so the order is the order they registered in).
    ///
    /// "Amazon ECS deploys exactly one daemon task on each container instance of the
    /// specified capacity providers" — an instance a placement cannot be made on at all
    /// ([`ContainerInstanceRecord::accepts_task_placement`], which excludes a `DRAINING`
    /// instance and one whose agent is gone) is not one the daemon task runs on.
    fn daemon_hosts(&self, cluster_name: &str) -> Vec<String> {
        let mut hosts: Vec<String> = self
            .container_instances
            .values()
            .filter(|instance| {
                instance.cluster_name == cluster_name && instance.accepts_task_placement()
            })
            .map(|instance| instance.arn.clone())
            .collect();
        hosts.sort();
        hosts
    }

    /// The LIVE tasks of one daemon — the ones it still wants running — by the container
    /// instance each of them was placed on.
    ///
    /// Built in ONE pass over the task map, so a pass that asks about every instance of a
    /// cluster costs one walk of the tasks rather than one per instance: what a single call
    /// costs stays bounded by what the store HOLDS ([`MAX_TASKS`]) rather than by the
    /// product of the instances and the tasks it holds.
    fn daemon_tasks_by_host(&self, daemon_arn: &str) -> HashMap<&str, &TaskRecord> {
        let mut by_host: HashMap<&str, &TaskRecord> = HashMap::new();
        for task in self.tasks.values() {
            if task.daemon_arn.as_deref() != Some(daemon_arn)
                || task.desired_status != STATUS_RUNNING
                || !task.is_active()
            {
                continue;
            }
            if let Some(host) = task.container_instance_arn.as_deref() {
                by_host.insert(host, task);
            }
        }
        by_host
    }

    /// Keep every ACTIVE daemon's task set in line with the container instances its
    /// capacity providers hold, as of `now`.
    ///
    /// This is the daemon scheduler's CONTINUOUS work, which is why the clock pass runs it:
    /// "Amazon ECS deploys exactly one daemon task on each container instance of the
    /// specified capacity providers. When a container instance registers with the cluster,
    /// Amazon ECS automatically starts daemon tasks." So the placement an instance that
    /// joined is owed was owed from the instant it joined, whether or not anybody called —
    /// and the daemon task of an instance that no longer accepts a placement is taken off
    /// it for the same reason.
    ///
    /// Nothing is placed for a daemon whose rollout has not started yet: the rollout is what
    /// deploys a revision ([`Self::advance_daemon_deployments`]), so a daemon created an
    /// instant ago holds no tasks until its initial deployment begins — which is what
    /// `DaemonDeployment` reports while it is [`DAEMON_DEPLOYMENT_PENDING`].
    ///
    /// A placement this emulator has no room for ([`MAX_TASKS`]) is left for later rather
    /// than failing the pass: the clock pass has no caller to answer.
    pub(super) fn maintain_daemon_tasks(&mut self, now: SystemTime) {
        let arns: Vec<String> = self.daemons.keys().cloned().collect();
        for daemon_arn in arns {
            let Some(daemon) = self.daemons.get(&daemon_arn).cloned() else {
                continue;
            };
            if daemon.status != DAEMON_ACTIVE || daemon.removal_due_at.is_some() {
                continue;
            }
            let Some(revision_arn) = daemon.current_revision_arn.clone() else {
                continue;
            };
            let rollout_started = self
                .daemon_deployments
                .get(&daemon.deployment_arn)
                .is_some_and(|deployment| deployment.started_at.is_some());
            if !rollout_started {
                continue;
            }
            // The Region the daemon's own ARNs carry, so a task it places names the Region
            // the daemon lives in rather than whichever Region last called.
            let region = daemon_region(&daemon);
            let hosts = self.daemon_hosts(&daemon.cluster_name);
            // What the daemon is running WHERE, read in one pass over the task map rather
            // than one per instance ([`Self::daemon_tasks_by_host`]), and as owned values so
            // the placements and stops below can write.
            let placed: HashMap<String, (String, Option<String>)> = self
                .daemon_tasks_by_host(&daemon.arn)
                .into_iter()
                .map(|(host, task)| {
                    (
                        host.to_string(),
                        (task.task_arn.clone(), task.daemon_revision_arn.clone()),
                    )
                })
                .collect();
            for host_arn in &hosts {
                match placed.get(host_arn) {
                    // An instance already running the revision being deployed needs
                    // nothing: one daemon task per instance is the whole of what a daemon
                    // wants there.
                    Some((_, Some(running))) if *running == revision_arn => continue,
                    // An instance running an EARLIER revision is one the rollout replaces:
                    // the task of the superseded revision is stopped and the new one placed
                    // in its stead.
                    Some((task_arn, _)) => {
                        let task_arn = task_arn.clone();
                        self.stop_service_task(&task_arn, now, DAEMON_TASK_REPLACED_REASON);
                        self.place_daemon_task(&daemon, &revision_arn, host_arn, &region, now);
                    }
                    None => {
                        self.place_daemon_task(&daemon, &revision_arn, host_arn, &region, now)
                    }
                }
            }
            // A daemon task whose host is no longer one the daemon runs on leaves with that
            // host: the instance is draining, its agent is gone, or its registration was
            // taken back, and a task on it is not one Amazon ECS is keeping there.
            let wanted: HashSet<&str> = hosts.iter().map(String::as_str).collect();
            let stranded: Vec<String> = placed
                .iter()
                .filter(|(host, _)| !wanted.contains(host.as_str()))
                .map(|(_, (task_arn, _))| task_arn.clone())
                .collect();
            for task_arn in stranded {
                self.stop_service_task(&task_arn, now, DAEMON_TASK_HOST_GONE_REASON);
            }
        }
    }

    /// Place ONE daemon task of `revision_arn` on the container instance `host_arn`:
    /// `PENDING` with its start owed by the clock [`TRANSITION_SPAN`] later, exactly as
    /// every other placement of this service leaves a task it placed.
    ///
    /// The task runs the daemon task definition the revision captured, so its containers
    /// are derived from that stored revision through the one minting path every placement
    /// of this service uses ([`Self::mint_containers`], over
    /// [`Self::daemon_container_definitions`]), and its `taskDefinition` is that revision's
    /// own ARN.
    fn place_daemon_task(
        &mut self,
        daemon: &DaemonRecord,
        revision_arn: &str,
        host_arn: &str,
        region: &str,
        now: SystemTime,
    ) {
        if self.tasks.len() >= MAX_TASKS {
            return;
        }
        let definition = self
            .daemon_task_definitions
            .values()
            .find(|definition| definition.arn == daemon.daemon_task_definition_arn)
            .cloned();
        // A revision this store no longer holds describes no containers, so there is
        // nothing to place: the daemon keeps whatever it is running until a later rollout
        // gives it a revision that exists.
        let Some(definition) = definition else {
            return;
        };
        let task_id = self.next_task_id();
        let task_arn = task_arn(region, &daemon.cluster_name, &task_id);
        let containers = self.mint_containers(
            region,
            &daemon.cluster_name,
            &task_id,
            &task_arn,
            &Self::daemon_container_definitions(&definition),
            STATUS_PENDING,
        );
        // Which of the daemon's capacity providers this placement is made on: the daemon
        // runs on the capacity its providers manage, and the scheduler spreads its
        // placements over them — the provider carrying the fewest of this daemon's tasks,
        // with the provider's own name breaking a tie, so one daemon's placements are
        // deterministic (the `spread` choice [`Self::select_task_host`] makes over
        // instances, one resource along).
        let capacity_provider_name = self.daemon_placement_provider(daemon);
        let record = TaskRecord {
            task_arn: task_arn.clone(),
            task_id,
            cluster_name: daemon.cluster_name.clone(),
            cluster_arn: daemon.cluster_arn.clone(),
            // A daemon task runs ON a container instance — "exactly one daemon task on each
            // container instance" — so the instance the scheduler placed it on is the one
            // it reports.
            container_instance_arn: Some(host_arn.to_string()),
            task_definition_arn: definition.arn.clone(),
            family: definition.family.clone(),
            last_status: STATUS_PENDING.to_string(),
            desired_status: STATUS_RUNNING.to_string(),
            containers,
            cpu: definition.cpu.clone(),
            memory: definition.memory.clone(),
            // A daemon's placement overrides nothing of the revision it runs, and names
            // each of that revision's containers ([`resolved_task_overrides`]).
            overrides: Some(resolved_task_overrides(
                None,
                &Self::daemon_container_definitions(&definition),
            )),
            // The capacity a daemon runs on is its capacity providers' ("ECS Managed
            // Daemons is only supported for Amazon ECS Managed Instances Capacity
            // Providers"), so a daemon task is placed through a provider rather than with a
            // launch type.
            launch_type: None,
            capacity_provider_name,
            // A daemon task runs on the capacity its providers manage rather than on
            // Fargate, so it reports none of the Fargate-only members; its containers run
            // on the architecture the instance under them reports for itself
            // ([`task_cpu_architecture`]).
            platform_version: None,
            platform_family: None,
            cpu_architecture: task_cpu_architecture(
                self.host_cpu_architecture(Some(host_arn)).as_deref(),
                None,
            ),
            availability_zone: self.task_availability_zone(Some(host_arn), &[]),
            ephemeral_storage_gib: None,
            // `Task$startedBy`: for a task the daemon scheduler placed, the deployment that
            // placed it.
            started_by: Some(daemon.deployment_arn.clone()),
            group: daemon_task_group(&daemon.name),
            service_name: None,
            daemon_arn: Some(daemon.arn.clone()),
            daemon_revision_arn: Some(revision_arn.to_string()),
            // "Specifies whether to propagate the tags from the daemon to the daemon
            // tasks. If you don't specify a value, the tags aren't propagated."
            tags: if daemon.propagate_tags == PROPAGATE_TAGS_DAEMON {
                daemon.tags.clone()
            } else {
                Vec::new()
            },
            enable_execute_command: daemon.enable_execute_command,
            created_at: DateTime::from(now),
            started_at: None,
            stopping_at: None,
            stopped_at: None,
            execution_stopped_at: None,
            pull_started_at: None,
            pull_stopped_at: None,
            stopped_reason: None,
            stop_code: None,
            version: 1,
            // The task is placed on a container instance that exists, so no capacity is
            // being waited for: only its own start is owed.
            pending_due_at: None,
            running_due_at: Some(now + TRANSITION_SPAN),
            stopped_due_at: None,
            protection_expires_at: None,
            // A daemon task shares the networking of the host that runs it: a daemon task
            // definition declares no network configuration to allocate an interface from.
            attachments: Vec::new(),
            connectivity: None,
            connectivity_at: None,
            // A daemon task definition declares no volume to configure at launch, and the
            // daemon scheduler's placement carries no volume configurations.
            volume_configurations: Vec::new(),
            awsvpc_configuration: None,
            launch_failure: None,
            launch_roles_assumed: false,
            // Nothing has retrieved the values this launch's containers take yet: that
            // happens once its roles are assumed and its images are fetched
            // ([`crate::state::settle_secret_retrievals`]).
            secrets_retrieved: false,
            execution_role_arn: definition.execution_role_arn.clone(),
            // A daemon's task is kept in place by the daemon scheduler
            // ([`Self::maintain_daemon_tasks`]), so no end of its work is owed: it runs for
            // as long as the instance under it does.
            work_done_due_at: None,
            // The credential its containers present is vended by the launch step that mints
            // one for the task role ([`crate::state::settle_task_role_credentials`]).
            task_role_credentials: None,
        };
        self.tasks.insert(task_arn, record);
    }

    /// Which of `daemon`'s capacity providers the next placement of it is made on.
    ///
    /// The daemon deploys onto the capacity its providers manage, and the scheduler
    /// spreads the placements over them: the provider carrying the fewest of this daemon's
    /// live tasks, with the provider's own name breaking a tie, so the placements of one
    /// daemon are deterministic. `None` for a daemon that names no provider at all, which
    /// a create refuses.
    fn daemon_placement_provider(&self, daemon: &DaemonRecord) -> Option<String> {
        daemon
            .capacity_provider_names
            .iter()
            .min_by_key(|name| {
                let placed = self
                    .tasks
                    .values()
                    .filter(|task| {
                        task.daemon_arn.as_deref() == Some(daemon.arn.as_str())
                            && task.desired_status == STATUS_RUNNING
                            && task.is_active()
                            && task.capacity_provider_name.as_deref() == Some(name.as_str())
                    })
                    .count();
                (placed, (*name).clone())
            })
            .cloned()
    }

    /// The container definitions of a DAEMON task definition, as the ONE shape every
    /// container record of this service is minted from ([`Self::mint_containers`]).
    ///
    /// `DaemonContainerDefinition` and `ContainerDefinition` describe a container the same
    /// way, member for member, for everything a container RECORD is built out of — the
    /// container's name and image, the CPU and memory it reserves, whether it is essential,
    /// what it waits on and the health check it declares — so a daemon's containers go
    /// through the same minting path rather than a second copy of it.
    fn daemon_container_definitions(
        definition: &DaemonTaskDefinitionRecord,
    ) -> Vec<ContainerDefinition> {
        definition
            .container_definitions
            .iter()
            .map(|container| ContainerDefinition {
                name: container.name.clone(),
                image: Some(container.image.clone()),
                cpu: container.cpu,
                memory: container.memory,
                memory_reservation: container.memory_reservation,
                essential: container.essential,
                depends_on: container.depends_on.clone(),
                health_check: container.health_check.clone(),
                ..ContainerDefinition::default()
            })
            .collect()
    }

    /// What ONE revision of ONE daemon adds up to on the container instances it deploys
    /// onto, counted off the task and container-instance maps.
    ///
    /// The tasks are indexed by their host in ONE pass ([`Self::daemon_tasks_by_host`]), so
    /// the count costs one walk of the task map and one of the instance map rather than one
    /// walk per instance.
    pub fn daemon_tally(&self, daemon: &DaemonRecord, revision_arn: &str) -> DaemonTally {
        let providers: Vec<String> = self
            .daemon_revisions
            .get(revision_arn)
            .map(|revision| revision.capacity_provider_names.clone())
            .unwrap_or_else(|| daemon.capacity_provider_names.clone());
        let mut tally = DaemonTally {
            by_provider: providers
                .iter()
                .map(|name| (name.clone(), 0, 0, 0))
                .collect(),
            ..DaemonTally::default()
        };
        let placed = self.daemon_tasks_by_host(&daemon.arn);
        for instance in self.container_instances.values() {
            if instance.cluster_name != daemon.cluster_name || !instance.is_registered() {
                continue;
            }
            // What this daemon runs on this instance, out of the one index built above.
            let on_host = placed.get(instance.arn.as_str()).copied();
            let task = on_host
                .filter(|task| task.daemon_revision_arn.as_deref() == Some(revision_arn));
            // An instance this revision has no task on is one running without the daemon
            // ONLY when no task of this daemon is on it at all: one running an earlier
            // revision is counted by that revision's own entry.
            if task.is_none() && on_host.is_some() {
                continue;
            }
            let standing = match (instance.status.as_str(), task) {
                (CONTAINER_INSTANCE_DRAINING, _) => HostStanding::Draining,
                (_, Some(task)) if task.last_status == STATUS_RUNNING => HostStanding::Running,
                (_, Some(_)) => HostStanding::Placing,
                (_, None) => HostStanding::WithoutDaemon,
            };
            // The provider the instance is counted under is the one the placement on it was
            // made through; an instance with no task of this revision is counted under the
            // revision's first provider, which is the one the next placement would spread
            // onto when the daemon names only one.
            let provider = task
                .and_then(|task| task.capacity_provider_name.clone())
                .or_else(|| providers.first().cloned());
            let entry = provider.and_then(|provider| {
                tally
                    .by_provider
                    .iter_mut()
                    .find(|(name, ..)| *name == provider)
            });
            match standing {
                HostStanding::Draining => {
                    tally.draining += 1;
                    if let Some((_, _, _, draining)) = entry {
                        *draining += 1;
                    }
                }
                HostStanding::Running => {
                    tally.running += 1;
                    if let Some((_, running, ..)) = entry {
                        *running += 1;
                    }
                }
                HostStanding::WithoutDaemon => {
                    tally.without_daemon += 1;
                    if let Some((_, _, without, _)) = entry {
                        *without += 1;
                    }
                }
                HostStanding::Placing => {}
            }
        }
        tally
    }

    /// The `DaemonRevisionDetail` a daemon's read reports for one revision it is running:
    /// the revision's ARN with the counts its tasks add up to.
    pub fn daemon_revision_detail(
        &self,
        daemon: &DaemonRecord,
        revision_arn: &str,
    ) -> DaemonRevisionDetail {
        let tally = self.daemon_tally(daemon, revision_arn);
        let region = daemon_region(daemon);
        DaemonRevisionDetail {
            arn: Some(revision_arn.to_string()),
            capacity_providers: Some(
                tally
                    .by_provider
                    .iter()
                    .map(|(name, running, without, _)| DaemonCapacityProvider {
                        arn: Some(capacity_provider_arn(&region, name)),
                        running_count: *running,
                        without_daemon_count: *without,
                    })
                    .collect(),
            ),
            total_running_count: tally.running,
            total_without_daemon_count: tally.without_daemon,
        }
    }

    /// The `DaemonDeploymentRevisionDetail` a deployment's read reports for one revision it
    /// moves the daemon between: the same counts, as that shape's own members name them.
    fn daemon_deployment_revision_detail(
        &self,
        daemon: &DaemonRecord,
        revision_arn: &str,
    ) -> DaemonDeploymentRevisionDetail {
        let tally = self.daemon_tally(daemon, revision_arn);
        let region = daemon_region(daemon);
        DaemonDeploymentRevisionDetail {
            arn: Some(revision_arn.to_string()),
            capacity_providers: Some(
                tally
                    .by_provider
                    .iter()
                    .map(
                        |(name, running, without, draining)| DaemonDeploymentCapacityProvider {
                            arn: Some(capacity_provider_arn(&region, name)),
                            running_instance_count: Some(*running),
                            without_daemon_instance_count: Some(*without),
                            draining_instance_count: Some(*draining),
                        },
                    )
                    .collect(),
            ),
            total_running_instance_count: Some(tally.running),
            total_without_daemon_instance_count: Some(tally.without_daemon),
            total_draining_instance_count: Some(tally.draining),
        }
    }

    /// The `DaemonDetail` `DescribeDaemon` reports for one stored daemon: what the create
    /// and every update wrote, with the revisions it is RUNNING and their counts read off
    /// the tasks that exist.
    ///
    /// `DaemonDetail$currentRevisions` is "The current daemon revision details, including
    /// the running task counts per capacity provider": the revision the daemon is deploying
    /// now, plus every earlier revision it still holds tasks of — which is what a rollout
    /// in progress looks like.
    pub fn daemon_detail(&self, daemon: &DaemonRecord) -> DaemonDetail {
        let mut revision_arns: Vec<String> =
            daemon.current_revision_arn.clone().into_iter().collect();
        for task in self.tasks.values() {
            if task.daemon_arn.as_deref() != Some(daemon.arn.as_str())
                || task.desired_status != STATUS_RUNNING
                || !task.is_active()
            {
                continue;
            }
            if let Some(arn) = task.daemon_revision_arn.clone() {
                if !revision_arns.contains(&arn) {
                    revision_arns.push(arn);
                }
            }
        }
        DaemonDetail {
            daemon_arn: Some(daemon.arn.clone()),
            cluster_arn: Some(daemon.cluster_arn.clone()),
            status: Some(daemon.status.clone()),
            current_revisions: Some(
                revision_arns
                    .iter()
                    .map(|arn| self.daemon_revision_detail(daemon, arn))
                    .collect(),
            ),
            deployment_arn: Some(daemon.deployment_arn.clone()),
            created_at: Some(daemon.created_at),
            updated_at: Some(daemon.updated_at),
        }
    }

    /// The `DaemonDeployment` `DescribeDaemonDeployments` reports for one stored rollout:
    /// what the call that started it wrote, with the revisions it moves the daemon between
    /// and their counts read off the task map.
    pub fn daemon_deployment_view(
        &self,
        deployment: &DaemonDeploymentRecord,
    ) -> DaemonDeployment {
        let daemon = self.daemons.get(&deployment.daemon_arn);
        DaemonDeployment {
            daemon_deployment_arn: Some(deployment.arn.clone()),
            cluster_arn: Some(deployment.cluster_arn.clone()),
            status: Some(deployment.status.clone()),
            status_reason: deployment.status_reason.clone(),
            // The revision this rollout is deploying, with the counts its tasks add up to —
            // and absent for the DRAIN a delete starts, which deploys none.
            target_daemon_revision: daemon.zip(deployment.target_revision_arn.as_deref()).map(
                |(daemon, target)| self.daemon_deployment_revision_detail(daemon, target),
            ),
            source_daemon_revisions: Some(
                daemon
                    .map(|daemon| {
                        deployment
                            .source_revision_arns
                            .iter()
                            .map(|arn| self.daemon_deployment_revision_detail(daemon, arn))
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            // `DaemonDeploymentConfiguration` carries no circuit-breaker configuration, so
            // no daemon deployment turns one on: "Amazon ECS is not using the circuit
            // breaker for daemon deployment failures when the status is `DISABLED`".
            circuit_breaker: Some(DaemonCircuitBreaker {
                failure_count: 0,
                status: Some(ROLLBACK_MONITORS_DISABLED.to_string()),
                threshold: 0,
            }),
            alarms: Some(deployment.alarms()),
            // `DaemonDeployment$rollback` is "the rollback options for the daemon
            // deployment", carried by exactly the rollouts a rollback ended — and nothing
            // rolls a daemon deployment back here, because no failure detector is turned
            // on to end one.
            rollback: None,
            deployment_configuration: deployment.deployment_configuration.clone(),
            created_at: Some(deployment.created_at),
            started_at: deployment.started_at,
            stopped_at: deployment.stopped_at,
            finished_at: deployment.finished_at,
        }
    }

    /// What the clock owes the DAEMON surface next: the earliest due instant a rollout or a
    /// requested delete holds, so the clock pump waits for it the way it waits for a task's
    /// own transitions ([`Self::next_task_due_at`]).
    pub fn next_daemon_due_at(&self) -> Option<SystemTime> {
        self.daemon_deployments
            .values()
            .flat_map(|deployment| [deployment.started_due_at, deployment.finished_due_at])
            .chain(self.daemons.values().map(|daemon| daemon.removal_due_at))
            .flatten()
            .min()
    }
}
