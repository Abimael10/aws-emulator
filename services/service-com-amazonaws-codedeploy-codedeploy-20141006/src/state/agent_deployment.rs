// SPDX-License-Identifier: CC0-1.0

//! The pass that drives an EC2/on-premises deployment's INSTANCE TARGETS: the one
//! place the agent's work on a deployment's fleet falls due.
//!
//! # What this pass does
//!
//! A deployment of the `Server` compute platform is work done on hosts, and the
//! hosts are not CodeDeploy's: which instances the deployment group selects belongs
//! to Amazon EC2 ([`super::instance_targets`]), and what the agent does on one of
//! them happens in that instance's own guest ([`super::instance_lifecycle`]). So
//! this pass is where the two meet — the fleet is resolved, each host is taken
//! through the deployment's lifecycle events, and what each one came to is written
//! back onto the deployment as its targets' own status, lifecycle events and
//! diagnostics.
//!
//! It runs where [`super::s3_revision`] and [`super::alarm_gate`] run, from
//! [`super::with_state`] with CodeDeploy's own store UNLOCKED: it reads and writes
//! two other services' stores, and the two are never held at once. And it runs for
//! the same reason: the work was owed at the instant the deployment started whether
//! or not anybody called CodeDeploy again, so a client that creates a deployment and
//! then only reads the hosts still sees it happen.
//!
//! # Once, and only when the fleet is there
//!
//! The run is performed ONCE per deployment: the hooks of a revision have effects
//! on a host, so nothing re-runs them on a later pass. A deployment whose hosts
//! have not finished coming up is left owing its run — nothing is performed and
//! nothing is written — until they have, or until the instant the deployment was
//! due to complete, at which point a host the agent could not reach is reported as
//! the `AGENT_ISSUE` the model names.
//!
//! # What the work COSTS the deployment
//!
//! What the agent does on a host takes however long that host's hooks stated they
//! took ([`InstanceRun::work_millis`]), and nothing of this process waits for it:
//! the time is REPORTED. So this pass records what each wave stated
//! ([`AgentDeployment::wave_work_ms`]) and the deployment's own window carries all of
//! it ([`stated_work_millis`], which is what the clock adds to the `completeTime` it
//! reports), while the lifecycle events of one host are stamped from the instant that
//! host's wave began on that same reported timeline. A deployment bounded to part of
//! its fleet works through more waves, and so spends the declared work more times,
//! than one that takes the whole fleet out of service at once.
//!
//! # What the outcome means
//!
//! A deployment whose group selected nothing has no host to install on and fails
//! with `NO_INSTANCES`, exactly as it did before this pass existed
//! ([`super::deployment_error_information`]). A deployment with hosts ends as those
//! hosts did: one whose every instance target succeeded succeeds, and one with a
//! failed instance target cannot report success for a revision that is not running
//! there — it fails with the `HEALTH_CONSTRAINTS` code CodeDeploy reports for a
//! deployment too few of whose instances took the revision.

use std::collections::HashMap;

use crate::ops::types::{ErrorInformation, RevisionLocation};

use super::appspec::{self, ServerDeclaration};
use super::instance_lifecycle::{self, InstanceArchive, InstanceOutcome, InstanceRun};
use super::revision_bundle::RevisionBundle;
use super::{
    check_on_premises_instance_name, deployment_is_terminal, rollback, service_state,
    ComAmazonawsCodedeployCodedeploy20141006State, Deployment, DeploymentTargetSnapshot,
    NameRefusal, DEPLOYMENT_TARGET_TYPE_INSTANCE, TARGET_STATUS_FAILED,
};

/// The `ComputePlatform` whose deployments are performed by an agent ON each target
/// instance (`crate::ops::types::COMPUTE_PLATFORM_VALUES`: ECS | Lambda | Server).
pub const COMPUTE_PLATFORM_SERVER: &str = "Server";

/// The `ErrorCode` a deployment too few of whose instances took the revision
/// carries (`smithy_query(query="shape",shape="ErrorCode")` lists it; the AWS
/// CodeDeploy User Guide's "Error codes" documents `HEALTH_CONSTRAINTS` as the
/// deployment failing because the instances it deployed to did not meet the
/// deployment configuration's health requirements).
pub const ERROR_CODE_HEALTH_CONSTRAINTS: &str = "HEALTH_CONSTRAINTS";

/// The `ErrorInformation$message` that goes with [`ERROR_CODE_HEALTH_CONSTRAINTS`]:
/// this failure's own fixed sentence, naming the three ways a fleet can fail to
/// take a revision and interpolating nothing from the deployment — every deployment
/// whose hosts did not take it fails the same way, and which host failed is what
/// the instance target's own diagnostics say.
pub const HEALTH_CONSTRAINTS_MESSAGE: &str =
    "The overall deployment failed because too many individual instances failed deployment, too \
     few healthy instances are available for deployment, or some instances in your deployment \
     group are experiencing problems.";

/// The `ErrorCode` of a deployment whose host never took the work
/// (`smithy_query(query="shape",shape="ErrorCode")` lists it; the AWS CodeDeploy
/// User Guide's "Error codes" documents `AGENT_ISSUE` as the agent not running on
/// the instance or not being able to communicate with CodeDeploy).
pub const ERROR_CODE_AGENT_ISSUE: &str = "AGENT_ISSUE";

/// The `ErrorInformation$message` that goes with [`ERROR_CODE_AGENT_ISSUE`]: a
/// fixed sentence saying what did not happen and where to look, interpolating
/// nothing — which host it was is what that host's own instance target reports.
pub const AGENT_ISSUE_MESSAGE: &str =
    "The deployment failed because the CodeDeploy agent did not receive the lifecycle event. \
     Check the CodeDeploy agent logs on your host and make sure the agent is running and can \
     connect to the CodeDeploy server.";

/// How many (deployment group, instance) pairs this store keeps a deployment
/// history for.
///
/// The history is what the NEXT deployment to a host needs ([`InstanceArchive`]),
/// so it outlives the deployment that wrote it; this bound is what keeps it from
/// growing without one, and it sits well above the fleet one account deploys to in
/// an emulator session. The oldest entries are dropped first.
const MAX_INSTANCE_ARCHIVES: usize = 512;

/// What the AGENT PLANE of one deployment holds: whether its work on the fleet is
/// still owed, and the revision that work installs.
///
/// Default — nothing owed, nothing to install — for every deployment of a platform
/// whose work is not done on hosts, so a Lambda or Amazon ECS deployment carries an
/// empty one.
#[derive(Debug, Clone, Default)]
pub struct AgentDeployment {
    /// Whether this deployment still owes the work the agent does on its fleet.
    pub awaits_run: bool,
    /// The revision's bundle, once it has been resolved: the AppSpec file and the
    /// files and scripts it names ([`super::revision_bundle`]).
    pub bundle: Option<RevisionBundle>,
    /// What that AppSpec declares ([`appspec::declared_server_in_app_spec`]).
    pub declaration: Option<ServerDeclaration>,
    /// Whether a host of the fleet never took the work, which is the
    /// [`ERROR_CODE_AGENT_ISSUE`] this deployment then reports.
    pub unreachable_host: bool,
    /// The hosts the agent has been round for this deployment: a pass goes round
    /// the hosts of the waves the clock has started ([`super::target_waves`]), so
    /// this is what keeps the next pass from performing a host's hooks a second
    /// time.
    pub performed: Vec<String>,
    /// How long the hooks of each WAVE's hosts stated they took, in milliseconds, by
    /// wave ([`super::instance_lifecycle::InstanceRun::work_millis`]).
    ///
    /// The hosts of one wave are deployed to together, so a wave took as long as its
    /// slowest host; the waves are worked through one at a time, so the deployment's
    /// own reported window carries all of them ([`stated_work_millis`]). Empty for a
    /// deployment no host of which has stated any work — every deployment of a
    /// revision whose hooks do not block.
    pub wave_work_ms: Vec<i64>,
}

/// How long `deployment`'s fleet STATED the work on it took, in milliseconds: the time
/// every wave of it spent, which is what the deployment's own window reports beyond the
/// spans this emulator gives a deployment ([`super::target_waves`]).
///
/// Zero for a deployment of a platform whose work is not done on hosts, and for one whose
/// revision declares no hook that takes any time.
#[must_use]
pub fn stated_work_millis(deployment: &Deployment) -> i64 {
    stated_through(
        &deployment.agent.wave_work_ms,
        deployment.agent.wave_work_ms.len(),
    )
}

impl AgentDeployment {
    /// What a deployment of `compute_platform` deploying `revision` owes its fleet.
    ///
    /// Only the platform whose work happens on hosts owes a run. A revision that
    /// carries its AppSpec in the REQUEST is read here, where the deployment is
    /// recorded, exactly as the other platforms' declarations are; one that names an
    /// object is fetched when the deployment starts, and what that object holds is
    /// written in then ([`super::s3_revision`]).
    #[must_use]
    pub fn of_revision(compute_platform: &str, revision: Option<&RevisionLocation>) -> Self {
        if compute_platform != COMPUTE_PLATFORM_SERVER {
            return Self::default();
        }
        let content = revision.and_then(appspec::revision_content);
        Self {
            awaits_run: true,
            bundle: content.map(RevisionBundle::of_app_spec),
            declaration: content.and_then(appspec::declared_server_in_app_spec),
            unreachable_host: false,
            performed: Vec::new(),
            wave_work_ms: Vec::new(),
        }
    }
}

/// Whether `deployment` may not be settled yet because the work on its fleet is
/// still owed.
///
/// A deployment's outcome FOLLOWS what its hosts did, so the clock does not give it
/// one before the agent has been round them ([`super::with_state`] runs this pass
/// before the clock, so a deployment whose run falls due in the same pass is
/// settled in that pass).
#[must_use]
pub fn blocks_completion(deployment: &Deployment) -> bool {
    deployment.agent.awaits_run
}

/// The failure an EC2/on-premises deployment's fleet came to, or `None` when its
/// hosts took the revision.
///
/// A host that never answered is reported as that, and otherwise a deployment with
/// a failed instance target carries the `HEALTH_CONSTRAINTS` failure: the revision
/// is not running where it was sent, so the deployment cannot report success for
/// it.
#[must_use]
pub fn instance_failure(deployment: &Deployment) -> Option<ErrorInformation> {
    if deployment.agent.unreachable_host {
        return Some(ErrorInformation {
            code: Some(ERROR_CODE_AGENT_ISSUE.to_string()),
            message: Some(AGENT_ISSUE_MESSAGE.to_string()),
        });
    }
    deployment
        .targets
        .iter()
        .any(|target| target.status.as_deref() == Some(TARGET_STATUS_FAILED))
        .then(|| ErrorInformation {
            code: Some(ERROR_CODE_HEALTH_CONSTRAINTS.to_string()),
            message: Some(HEALTH_CONSTRAINTS_MESSAGE.to_string()),
        })
}

/// The targets a ROLLBACK of a deployment starts with: the very fleet that
/// deployment went to, with what each host came to on it cleared.
///
/// A rollback is a deployment to the same deployment group over the same fleet — it
/// inherits the targets and the waves they were paced in — but none of them has
/// taken the revision it redeploys yet, so the status, the lifecycle events and the
/// instant of the run the rolled-back deployment recorded on each host are not the
/// rollback's to report: the agent goes round them again ([`super::instance_lifecycle`]).
#[must_use]
pub fn fleet_to_resolve_again(
    targets: &[DeploymentTargetSnapshot],
) -> Vec<DeploymentTargetSnapshot> {
    targets
        .iter()
        .map(|target| DeploymentTargetSnapshot {
            status: None,
            lifecycle_events: Vec::new(),
            updated_at_ms: None,
            ..target.clone()
        })
        .collect()
}

/// Perform the work every EC2/on-premises deployment owes the hosts its waves have
/// reached at `now_ms`.
///
/// A store holding no such deployment costs one read of this store and no other
/// store's lock at all.
///
/// The agent goes round a host ONCE, and only once the wave that deploys to it has
/// started ([`super::target_waves`]): the fleet was resolved when the deployment was
/// created ([`super::ec2_fleet`]) and cut into waves by its configuration, so this
/// pass performs the hosts of the waves the clock has reached and leaves the rest to
/// a later pass. That is also what bounds one pass's work and gives a long fleet
/// somewhere to be interrupted: the pass returns, every store is released, and the
/// deployment is not settled until the agent has been round all of it
/// ([`blocks_completion`]).
///
/// `now` is the instant of the whole pass this is one stage of, handed in by
/// [`super::with_state`] so every stage of that pass asks what was due at ONE
/// instant; see that function.
pub fn drive_due_instance_targets<Eng: Send + Sync + 'static>(engine: &Eng, now_ms: i64) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let due: Vec<DueRun> = {
        let store = state.lock();
        due_runs(&store, now_ms)
    };
    for due in due {
        // The hosts of the waves this deployment has reached that the agent has not
        // been round yet, in wave order.
        let going_round: Vec<String> = due
            .hosts
            .iter()
            .filter(|host| host.wave <= due.waves_started)
            .filter(|host| !due.performed.contains(&host.instance_id))
            .map(|host| host.instance_id.clone())
            .collect();
        // A host whose operating system has not come up takes nothing yet: until it
        // has — or the deployment was due to complete — the run of that host stays
        // owed and nothing is performed on it.
        let ready = going_round
            .iter()
            .all(|instance_id| instance_lifecycle::host_is_up(engine, instance_id));
        if !ready && now_ms < due.completes_at_ms {
            continue;
        }
        let mut runs: Vec<InstanceRun> = Vec::new();
        let mut unreachable: Vec<String> = Vec::new();
        // What the waves of this deployment have stated they took, by wave, UPDATED as this
        // pass goes round them: a pass that drives several waves at once — which is every
        // pass that comes a wave span or more after the one before it — has to stamp a later
        // wave's events after the work of the waves before it, including the waves it has
        // just performed itself.
        let mut stated = due.wave_work_ms.clone();
        for instance_id in &going_round {
            let previous = due.previous.get(instance_id);
            // Where this host's run begins on the deployment's own reported timeline: the
            // waves before its own were worked through first, and what they STATED they
            // took is time this deployment reports ([`stated_work_millis`]), so a host of
            // a later wave begins after them rather than beside them.
            let wave = due.wave_of(instance_id).unwrap_or(0);
            let began_ms = now_ms.saturating_add(stated_through(&stated, wave));
            match instance_lifecycle::perform_on(
                engine,
                &due.work,
                instance_id,
                previous,
                began_ms,
            ) {
                InstanceOutcome::NoGuest => unreachable.push(instance_id.clone()),
                InstanceOutcome::Performed(run) => {
                    // The hosts of one wave are deployed to together, so the wave took as
                    // long as its slowest host.
                    if stated.len() <= wave {
                        stated.resize(wave.saturating_add(1), 0);
                    }
                    stated[wave] = stated[wave].max(run.work_millis);
                    runs.push(run);
                }
            }
        }
        // Whether the agent has now been round the WHOLE fleet, which is what the
        // deployment's own outcome waits for.
        let round = due.hosts.len() <= due.performed.len() + going_round.len();
        let mut store = state.lock();
        record(&mut store, &due, &runs, &unreachable, round, now_ms);
    }
}

/// One host of a deployment's fleet, as the pass below asks about it: the instance
/// the agent goes round, and the wave of the deployment that deploys to it.
#[derive(Debug, Clone)]
struct FleetHost {
    /// The instance id, which is the target id the deployment reports it under.
    instance_id: String,
    /// Which wave of the deployment deploys to it ([`super::target_waves`]).
    wave: usize,
}

/// One deployment's owed run, as the question is asked of the other services.
#[derive(Debug, Clone)]
struct DueRun {
    /// The deployment.
    deployment_id: String,
    /// The Amazon EC2 hosts of its fleet, in the order its targets were resolved.
    hosts: Vec<FleetHost>,
    /// How many waves of those hosts the deployment has started, which is the last
    /// wave the agent may go round at this instant.
    waves_started: usize,
    /// Everything the agent needs to perform it on one host.
    work: instance_lifecycle::AgentWork,
    /// What the last successful deployment of this group left on each host of the
    /// fleet, by instance id.
    previous: HashMap<String, InstanceArchive>,
    /// The hosts the agent has already been round for this deployment, so a pass
    /// goes round the rest rather than performing one host's hooks twice.
    performed: Vec<String>,
    /// How long the waves already performed stated they took, by wave
    /// ([`AgentDeployment::wave_work_ms`]): where the wave this pass performs begins on
    /// the deployment's own reported timeline.
    wave_work_ms: Vec<i64>,
    /// The instant the deployment was due to complete, which is how long a host
    /// that has not come up yet is waited for.
    completes_at_ms: i64,
}

impl DueRun {
    /// Which wave of this deployment deploys to `instance_id`, when its fleet holds it.
    fn wave_of(&self, instance_id: &str) -> Option<usize> {
        self.hosts
            .iter()
            .find(|host| host.instance_id == instance_id)
            .map(|host| host.wave)
    }
}

/// How long the first `waves` waves of `wave_work_ms` stated they took, in milliseconds:
/// where the wave after them begins on a deployment's own reported timeline.
fn stated_through(wave_work_ms: &[i64], waves: usize) -> i64 {
    wave_work_ms
        .iter()
        .take(waves)
        .fold(0_i64, |total, work| total.saturating_add(*work))
}

/// The run every deployment that owes one at `now_ms` owes.
fn due_runs(
    state: &ComAmazonawsCodedeployCodedeploy20141006State,
    now_ms: i64,
) -> Vec<DueRun> {
    state
        .pending_deployments
        .iter()
        .filter_map(|id| state.deployments.get(id))
        .filter(|deployment| is_due(deployment, now_ms))
        .map(|deployment| due_run_of(state, deployment))
        .collect()
}

/// Whether `deployment` is one whose fleet work is due at `now_ms`.
///
/// It owes one, it has reached no outcome, the clock has started it, and its
/// revision has been resolved: what the agent installs is what that revision's
/// AppSpec declares, so nothing is performed before the fetch that reads it
/// ([`super::s3_revision`] runs earlier in the same pass).
fn is_due(deployment: &Deployment, now_ms: i64) -> bool {
    deployment.agent.awaits_run
        && !deployment.awaits_revision_fetch
        && deployment.complete_time_ms.is_none()
        && !deployment_is_terminal(&deployment.status)
        && deployment.starts_at_ms <= now_ms
}

/// What one due deployment asks of the other services.
///
/// Its FLEET is the instance targets it already holds: the Amazon EC2 instances its
/// group selected when the deployment was created ([`super::ec2_fleet`]), each in
/// the wave its configuration put it in. An on-premises target is not one of them —
/// the agent of this emulator reaches an instance's guest, and the guest of a host
/// this emulator holds no instance for is not something it can reach — so it is
/// left where [`super::target_waves`] takes it.
///
/// A deployment whose group is no longer stored still performs the fleet it
/// resolved: the hosts are on the deployment's own record, so dropping the group
/// changes nothing about where this deployment was going.
fn due_run_of(
    state: &ComAmazonawsCodedeployCodedeploy20141006State,
    deployment: &Deployment,
) -> DueRun {
    let group = rollback::group_of(state, deployment);
    let deployment_group_id = group
        .map(|group| group.deployment_group_id.clone())
        .unwrap_or_default();
    let hosts: Vec<FleetHost> = deployment
        .targets
        .iter()
        .filter(|target| target.target_type == DEPLOYMENT_TARGET_TYPE_INSTANCE)
        .filter_map(|target| {
            let instance_id = target.target_id.clone()?;
            is_ec2_instance_id(&instance_id).then_some(FleetHost {
                instance_id,
                wave: target.wave,
            })
        })
        .collect();
    let previous = state
        .instance_archives
        .iter()
        .filter(|((group_id, _), _)| group_id == &deployment_group_id)
        .map(|((_, instance_id), archive)| (instance_id.clone(), archive.clone()))
        .collect();
    DueRun {
        deployment_id: deployment.deployment_id.clone(),
        hosts,
        waves_started: deployment.target_waves_finished,
        work: instance_lifecycle::AgentWork {
            deployment_id: deployment.deployment_id.clone(),
            application_name: deployment.application_name.clone(),
            deployment_group_name: deployment
                .deployment_group_name
                .clone()
                .unwrap_or_default(),
            deployment_group_id,
            bundle: deployment.agent.bundle.clone().unwrap_or_default(),
            declaration: deployment.agent.declaration.clone().unwrap_or_default(),
            file_exists_behavior: deployment
                .file_exists_behavior
                .clone()
                .unwrap_or_default(),
            ignore_application_stop_failures: deployment.ignore_application_stop_failures,
        },
        previous,
        performed: deployment.agent.performed.clone(),
        wave_work_ms: deployment.agent.wave_work_ms.clone(),
        completes_at_ms: deployment.completes_at_ms,
    }
}

/// Write back what the hosts this pass went round came to.
///
/// This store was unlocked while the other two were read and written, so what the
/// run was started from is checked again: a deployment another caller stopped, or
/// the clock settled, in between keeps the outcome it already has and the hosts'
/// own records are dropped. Each host is recorded as gone round, so one host's
/// hooks run once however many passes go by, and the deployment stops owing the
/// run once `round` says the agent has been round the whole fleet.
fn record(
    state: &mut ComAmazonawsCodedeployCodedeploy20141006State,
    due: &DueRun,
    runs: &[InstanceRun],
    unreachable: &[String],
    round: bool,
    now_ms: i64,
) {
    if !state
        .deployments
        .get(&due.deployment_id)
        .is_some_and(|deployment| is_due(deployment, now_ms))
    {
        return;
    }
    let group_id = due.work.deployment_group_id.clone();
    let Some(deployment) = state.deployments.get_mut(&due.deployment_id) else {
        return;
    };
    deployment.agent.awaits_run = !round;
    deployment.agent.unreachable_host =
        deployment.agent.unreachable_host || !unreachable.is_empty();
    // What each host the agent went round came to, written onto the target the
    // deployment already holds for it: the fleet itself was resolved when the
    // deployment was created ([`super::ec2_fleet`]), so nothing is added here.
    for run in runs {
        if let Some(target) = target_of(deployment, &run.instance_id) {
            target.status = Some(run.status.to_string());
            target.lifecycle_events = run.events.clone();
            target.updated_at_ms = Some(now_ms);
        }
        deployment.agent.performed.push(run.instance_id.clone());
        // And how long that host's hooks stated they took, onto the WAVE it is in: the
        // wave is not finished until the agent is finished with its hosts, and they are
        // deployed to together, so the wave takes as long as its slowest host.
        if let Some(wave) = due.wave_of(&run.instance_id) {
            let work = &mut deployment.agent.wave_work_ms;
            if work.len() <= wave {
                work.resize(wave.saturating_add(1), 0);
            }
            work[wave] = work[wave].max(run.work_millis);
        }
    }
    // The window the deployment is held open for is the one its waves were given: the
    // work the hooks STATED they took is reported rather than waited for
    // ([`stated_work_millis`]), so nothing here moves the instant the clock settles this
    // deployment at.
    for instance_id in unreachable {
        // A host that never took the work reports the status of a target the
        // revision did not reach, and no lifecycle event of its own because none
        // was run on it.
        if let Some(target) = target_of(deployment, instance_id) {
            target.status = Some(TARGET_STATUS_FAILED.to_string());
            target.updated_at_ms = Some(now_ms);
        }
        deployment.agent.performed.push(instance_id.clone());
    }
    // What each host that TOOK the revision now holds, which is what the next
    // deployment to it reads: the archive its ApplicationStop runs out of and the
    // files that "were part of the previous successful deployment".
    for run in runs {
        if run.status != TARGET_STATUS_FAILED {
            state.record_instance_archive(
                &group_id,
                &run.instance_id,
                InstanceArchive {
                    deployment_id: due.deployment_id.clone(),
                    installed: run.installed.clone(),
                },
            );
        }
    }
}

/// Whether `target_id` names an Amazon EC2 instance, which is the target of a
/// `Server` deployment the agent of this emulator can reach: the guest it performs
/// the work in belongs to that service's instance.
///
/// The form is the one this service RESERVES for an instance id — no on-premises
/// host may register under it ([`check_on_premises_instance_name`]) — so it is the
/// same reading that decides which ARN a target carries
/// ([`super::deployment_target_arn`]).
fn is_ec2_instance_id(target_id: &str) -> bool {
    matches!(
        check_on_premises_instance_name(target_id),
        Err(NameRefusal::Reserved)
    )
}

/// The target `deployment` reports `instance_id` under, for writing what that host
/// came to onto it.
fn target_of<'a>(
    deployment: &'a mut Deployment,
    instance_id: &str,
) -> Option<&'a mut DeploymentTargetSnapshot> {
    deployment
        .targets
        .iter_mut()
        .find(|target| target.target_id.as_deref() == Some(instance_id))
}

impl ComAmazonawsCodedeployCodedeploy20141006State {
    /// Record what the deployment `archive` names left on `instance_id` for the
    /// deployment group `group_id`, keeping the history bounded by
    /// [`MAX_INSTANCE_ARCHIVES`].
    pub fn record_instance_archive(
        &mut self,
        group_id: &str,
        instance_id: &str,
        archive: InstanceArchive,
    ) {
        let key = (group_id.to_string(), instance_id.to_string());
        if !self.instance_archives.contains_key(&key)
            && self.instance_archives.len() >= MAX_INSTANCE_ARCHIVES
        {
            // Room is made by dropping the history of another host, which only
            // costs that host its ApplicationStop on the next deployment.
            if let Some(oldest) = self.instance_archive_order.first().cloned() {
                self.instance_archives.remove(&oldest);
                self.instance_archive_order.remove(0);
            }
        }
        if self.instance_archives.insert(key.clone(), archive).is_none() {
            self.instance_archive_order.push(key);
        }
    }
}
