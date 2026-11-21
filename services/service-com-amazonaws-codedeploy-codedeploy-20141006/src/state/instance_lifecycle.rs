// SPDX-License-Identifier: CC0-1.0

//! What the CodeDeploy AGENT does on one Amazon EC2 instance: the lifecycle events
//! of an EC2/on-premises deployment, performed in that instance's own guest.
//!
//! # What such a deployment IS
//!
//! On the `Server` compute platform the deployment is work done ON THE HOST: "the
//! CodeDeploy agent … is a software package that, when installed and configured on
//! an instance, makes it possible for that instance to be used in CodeDeploy
//! deployments", and what it does there is run the deployment's lifecycle events in
//! order — ApplicationStop, DownloadBundle, BeforeInstall, Install, AfterInstall,
//! ApplicationStart and ValidateService (the AWS CodeDeploy User Guide's
//! "Lifecycle event hook availability" for an in-place deployment with no load
//! balancer) — copying the revision's files onto the host at Install and running
//! the script the AppSpec's `hooks` section names for each of the other events. A
//! deployment that only recorded a status would have installed nothing: the client
//! reads the effect back off the host.
//!
//! # Where it happens
//!
//! The agent is INSIDE the guest, and the guest belongs to the service that owns
//! instances: its storage and the shell a program of it runs in are facts of the
//! Amazon EC2 instance. So this module asks that service to put the bundle on the
//! host and to perform each hook script there, through its own seams
//! (`ComAmazonawsEc2Amazonec2State::put_in_instance_guest` and
//! `::run_in_instance_guest_with`, which state what a run is and what it is not) —
//! the same bounded, in-process shell session the instance's own boot script runs
//! in. There is no process, no socket and no host path in it, a command the session
//! provides no program for answers the status a shell answers for a utility it
//! could not find, and nothing is reported for an instance whose guest is not up.
//!
//! # What the host keeps
//!
//! The agent keeps the revision it deployed on the host, under its own deployment
//! root ([`archive_directory`], the layout the agent uses:
//! `/opt/codedeploy-agent/deployment-root/<deployment-group-id>/<deployment-id>/deployment-archive`).
//! That is what makes a REDEPLOY different from a first deployment: "the AppSpec
//! file and scripts used for the ApplicationStop … are the ones from the previously
//! deployed revision", so this module reads that archive off the host rather than
//! keeping a second copy of a past revision in CodeDeploy's own state. An instance
//! no revision has been deployed to holds no archive, which is why ApplicationStop
//! is skipped the first time.
//!
//! # The environment a hook runs in
//!
//! "CodeDeploy makes the following environment variables available during each
//! deployment lifecycle event": `APPLICATION_NAME`, `DEPLOYMENT_ID`,
//! `DEPLOYMENT_GROUP_NAME`, `DEPLOYMENT_GROUP_ID` and `LIFECYCLE_EVENT` (the AWS
//! CodeDeploy User Guide's "Environment variable availability"). Each is exported
//! in the session the script runs in, with the value of THIS deployment's own
//! records, so a script that reads one reads the deployment it is part of.
//!
//! # How long a hook takes
//!
//! A hook is the client's own program, and the time it takes is the time it STATES it took
//! (`ComAmazonawsEc2Amazonec2State::run_in_instance_guest_with` answers it as
//! `GuestRun::took`, since nothing of this process waits for a program). That time is the
//! agent's, and it is REPORTED rather than waited for: the lifecycle events of one host
//! occupy the run in order, each one's window beginning where the one before it ended
//! ([`InstanceRun::work_millis`]), and the deployment's own window carries what every wave of
//! its fleet stated ([`super::agent_deployment::stated_work_millis`]) — so a revision whose
//! hook blocks is a deployment that reports it took that long.

use ::service_com_amazonaws_ec2_amazonec2::state as instances;

use crate::ops::types::{Diagnostics, LifecycleEvent};

use super::appspec::{self, DeclaredScript, ServerDeclaration};
use super::deployment_summary::{
    FILE_EXISTS_BEHAVIOR_OVERWRITE, FILE_EXISTS_BEHAVIOR_RETAIN,
};
use super::revision_bundle::{
    entry_path, RevisionBundle, APP_SPEC_NAMES, MAX_BUNDLE_BYTES, MAX_BUNDLE_ENTRIES,
};
use super::{
    timestamp, LIFECYCLE_EVENT_STATUS_FAILED, LIFECYCLE_EVENT_STATUS_SKIPPED,
    LIFECYCLE_EVENT_STATUS_SUCCEEDED, TARGET_STATUS_FAILED, TARGET_STATUS_SUCCEEDED,
};

/// The lifecycle event at which "the application is stopped before the new
/// revision is installed"; see the module docs for the sequence.
pub const LIFECYCLE_EVENT_APPLICATION_STOP: &str = "ApplicationStop";
/// See [`LIFECYCLE_EVENT_APPLICATION_STOP`]: the event at which the agent puts the
/// revision's bundle on the host. It is reserved for the agent — "the
/// `DownloadBundle` … events are reserved for CodeDeploy operations and cannot be
/// used to run scripts".
pub const LIFECYCLE_EVENT_DOWNLOAD_BUNDLE: &str = "DownloadBundle";
/// See [`LIFECYCLE_EVENT_APPLICATION_STOP`].
pub const LIFECYCLE_EVENT_BEFORE_INSTALL: &str = "BeforeInstall";
/// See [`LIFECYCLE_EVENT_APPLICATION_STOP`]: the event at which the agent copies
/// the revision's files to their destinations, also reserved for the agent.
pub const LIFECYCLE_EVENT_INSTALL: &str = "Install";
/// See [`LIFECYCLE_EVENT_APPLICATION_STOP`].
pub const LIFECYCLE_EVENT_AFTER_INSTALL: &str = "AfterInstall";
/// See [`LIFECYCLE_EVENT_APPLICATION_STOP`].
pub const LIFECYCLE_EVENT_APPLICATION_START: &str = "ApplicationStart";
/// See [`LIFECYCLE_EVENT_APPLICATION_STOP`].
pub const LIFECYCLE_EVENT_VALIDATE_SERVICE: &str = "ValidateService";

/// The lifecycle events an in-place EC2/on-premises deployment runs, in the order
/// the agent runs them.
///
/// The sequence of an in-place deployment that uses no load balancer, which is the
/// deployment this emulator performs: the events a load balancer adds
/// (BeforeBlockTraffic, BlockTraffic, AfterBlockTraffic and the AllowTraffic trio)
/// are about traffic being taken off and put back on a host, which needs a
/// registered target to take it off.
pub const SERVER_LIFECYCLE_EVENTS: &[&str] = &[
    LIFECYCLE_EVENT_APPLICATION_STOP,
    LIFECYCLE_EVENT_DOWNLOAD_BUNDLE,
    LIFECYCLE_EVENT_BEFORE_INSTALL,
    LIFECYCLE_EVENT_INSTALL,
    LIFECYCLE_EVENT_AFTER_INSTALL,
    LIFECYCLE_EVENT_APPLICATION_START,
    LIFECYCLE_EVENT_VALIDATE_SERVICE,
];

/// The `LifecycleErrorCode` of a script that ran
/// (`smithy_query(query="doc",shape="Diagnostics",member="errorCode")`: "Success:
/// The specified script ran").
pub const LIFECYCLE_ERROR_CODE_SUCCESS: &str = "Success";
/// See [`LIFECYCLE_ERROR_CODE_SUCCESS`]: "ScriptMissing: The specified script was
/// not found in the specified location".
pub const LIFECYCLE_ERROR_CODE_SCRIPT_MISSING: &str = "ScriptMissing";
/// See [`LIFECYCLE_ERROR_CODE_SUCCESS`]: "ScriptFailed: The specified script failed
/// to run as expected".
pub const LIFECYCLE_ERROR_CODE_SCRIPT_FAILED: &str = "ScriptFailed";
/// See [`LIFECYCLE_ERROR_CODE_SUCCESS`]: "UnknownError: The specified script did
/// not run for an unknown reason" — the code of a lifecycle event the agent itself
/// performs (DownloadBundle, Install) and could not complete, since no script of
/// the revision was involved in it.
pub const LIFECYCLE_ERROR_CODE_UNKNOWN: &str = "UnknownError";

/// How many bytes of a script's output one lifecycle event reports: "If available,
/// CodeDeploy returns up to the last 4 KB of the diagnostic log"
/// (`smithy_query(query="doc",shape="Diagnostics",member="logTail")`).
const MAX_LOG_TAIL_BYTES: usize = 4 * 1024;

/// How many files ONE Install resolves, and how many bytes they come to.
///
/// The revision is a client's object and its AppSpec a client's document, and a
/// `files` section may name the SAME bundle entry from several of its entries — so
/// what an Install resolves is bounded by the PRODUCT and not only by each factor:
/// the copies are counted and measured as they are resolved, and the event fails
/// once they pass what the unpacked bundle itself is held to
/// ([`MAX_BUNDLE_ENTRIES`], [`MAX_BUNDLE_BYTES`]), which is also more than a host's
/// own storage would take.
const MAX_INSTALLED_FILES: usize = MAX_BUNDLE_ENTRIES;

/// See [`MAX_INSTALLED_FILES`].
const MAX_INSTALLED_BYTES: usize = MAX_BUNDLE_BYTES;

/// How much STATED hook time one host's run may report.
///
/// A hook script is a client's own program and what it says it took is what the agent spent
/// on it ([`::service_com_amazonaws_ec2_amazonec2::state::GuestRun::took`]), so the work one
/// revision can have a deployment report for one host is bounded HERE, the way the spans a
/// paced deployment is held open for are bounded ([`super::target_waves::MAX_RUN_SPANS`]):
/// the same ceiling the host's own session holds one program's `sleep` to
/// ([`::service_common::posix_shell_session::Bounds::sleep_seconds`]), so a revision cannot
/// have one host's run report more time than one of its scripts could ask for.
const MAX_HOST_WORK_MILLIS: i64 = 300_000;

/// The directory the agent keeps one deployment's revision in on the host.
///
/// The layout the CodeDeploy agent uses for its deployment root:
/// `/opt/codedeploy-agent/deployment-root/<deployment-group-id>/<deployment-id>/deployment-archive`,
/// which is where a hook script's own path is resolved from and where the archive of
/// the last deployment is still found on a redeploy.
#[must_use]
pub fn archive_directory(deployment_group_id: &str, deployment_id: &str) -> String {
    format!(
        "/opt/codedeploy-agent/deployment-root/{deployment_group_id}/{deployment_id}/\
         deployment-archive"
    )
}

/// What one deployment left on one instance: the deployment whose archive the host
/// holds, and the destinations its Install copied files to.
///
/// Held per (deployment group, instance) by CodeDeploy's own store, because it is
/// what the NEXT deployment to that instance needs: which archive ApplicationStop
/// runs out of, and which files on the host "were part of the previous successful
/// deployment" — the very distinction `fileExistsBehavior` is about.
#[derive(Debug, Clone, Default)]
pub struct InstanceArchive {
    /// The deployment whose archive is on the host.
    pub deployment_id: String,
    /// The paths that deployment installed, as the host holds them.
    pub installed: Vec<String>,
}

/// Everything about one deployment the agent needs to perform it, taken off the
/// stored deployment while CodeDeploy's store is held so the instances' guests can
/// be reached with it unlocked.
#[derive(Debug, Clone, Default)]
pub struct AgentWork {
    /// The deployment being performed, which is also part of the path its archive
    /// is kept under and the `DEPLOYMENT_ID` its hooks read.
    pub deployment_id: String,
    /// `APPLICATION_NAME`.
    pub application_name: String,
    /// `DEPLOYMENT_GROUP_NAME`.
    pub deployment_group_name: String,
    /// `DEPLOYMENT_GROUP_ID`, which is also part of the archive's path.
    pub deployment_group_id: String,
    /// The revision's bundle: the AppSpec file, and the files and scripts it names.
    pub bundle: RevisionBundle,
    /// What that AppSpec declares.
    pub declaration: ServerDeclaration,
    /// The deployment's effective `fileExistsBehavior`.
    pub file_exists_behavior: String,
    /// Its `ignoreApplicationStopFailures`.
    pub ignore_application_stop_failures: bool,
}

/// What the agent's run on one instance came to.
#[derive(Debug, Clone)]
pub struct InstanceRun {
    /// The instance the run was performed on.
    pub instance_id: String,
    /// The `TargetStatus` the instance target reached.
    pub status: &'static str,
    /// Its lifecycle events, one per step of the sequence, in order.
    pub events: Vec<LifecycleEvent>,
    /// The paths this deployment installed on the host.
    pub installed: Vec<String>,
    /// How long the hook scripts of this run STATED they took, in milliseconds: the time the
    /// agent spent on this host, which the deployment's own window then reports
    /// ([`super::agent_deployment::stated_work_millis`]) and which the lifecycle events below
    /// report as their own windows. Bounded by [`MAX_HOST_WORK_MILLIS`].
    pub work_millis: i64,
}

/// What asking one instance to perform a deployment answered.
#[derive(Debug, Clone)]
pub enum InstanceOutcome {
    /// There is no guest to perform anything in yet: the instance's operating
    /// system has not come up, or the service that owns it is not on this engine.
    /// Nothing was performed and nothing was written.
    NoGuest,
    /// The agent performed the deployment's lifecycle events on the host.
    Performed(InstanceRun),
}

/// Whether the instance `instance_id` names has a host the agent can act on now:
/// Amazon EC2 holds it running and its operating system has come up.
///
/// The moves that service already owed its instances are materialised first, so a
/// launch whose boot had fallen due is a host with an operating system on it
/// whether or not anybody called Amazon EC2.
pub fn host_is_up<Eng: Send + Sync + 'static>(engine: &Eng, instance_id: &str) -> bool {
    let Some(state) = instances::service_state(engine) else {
        return false;
    };
    let mut store = state.lock();
    store.materialize_due_transitions();
    store.instance_guest_is_up(instance_id)
}

/// Perform `work` on the instance `instance_id` names, as the agent on that host
/// performs it, answering what the run came to.
///
/// `previous` is what the last deployment of this deployment group left on this
/// instance, where one has.
///
/// The machine's own IDENTITY is resolved BEFORE the instance store is taken (which
/// role its instance profile carries is a fact IAM holds), the same way every
/// caller that reaches into a guest resolves it, and the moves Amazon EC2 already
/// owed its instances are materialised first, so a host whose boot had fallen due
/// is a host with an operating system on it.
///
/// The CREDENTIALS of that role are not published to a hook: a session of it is
/// minted by the token service out of the emulator's own credential store, which
/// only a REQUEST's context carries, and this work falls due on the clock whether
/// or not anybody is calling. A guest the agent reaches therefore publishes the
/// role's name and no credentials for it, rather than a session nothing minted.
///
/// `began_ms` is where this run begins on the deployment's own reported timeline — the
/// instant of the pass, plus whatever the waves before this host's stated they took — which
/// is what the lifecycle events of this run are stamped from (see the module docs).
pub fn perform_on<Eng: Send + Sync + 'static>(
    engine: &Eng,
    work: &AgentWork,
    instance_id: &str,
    previous: Option<&InstanceArchive>,
    began_ms: i64,
) -> InstanceOutcome {
    let identity = instances::GuestIdentity {
        iam_role_name: instances::instance_profile_role_name(engine, instance_id),
        iam_role_credentials: None,
    };
    let Some(state) = instances::service_state(engine) else {
        return InstanceOutcome::NoGuest;
    };
    let mut store = state.lock();
    store.materialize_due_transitions();
    if !store.instance_guest_is_up(instance_id) {
        return InstanceOutcome::NoGuest;
    }
    let mut agent = Agent {
        store: &mut store,
        instance_id,
        work,
        identity,
        began_ms,
        work_millis: 0,
    };
    InstanceOutcome::Performed(agent.perform(previous))
}

/// The agent of one host, performing one deployment on it.
///
/// It asks the service that owns the host for exactly four things, and for nothing
/// else: what the host holds at a path, whether it holds anything there, the files
/// it is to keep, and one program performed in its guest.
struct Agent<'a> {
    /// The instance store, whose guest seams do the work.
    store: &'a mut instances::ComAmazonawsEc2Amazonec2State,
    /// The host.
    instance_id: &'a str,
    /// The deployment.
    work: &'a AgentWork,
    /// What the guest's metadata service publishes about the role it runs as.
    identity: instances::GuestIdentity,
    /// Where this run began on the deployment's own reported timeline.
    began_ms: i64,
    /// How long the hooks performed so far stated they took, which is how far into the run
    /// the next lifecycle event begins; see [`InstanceRun::work_millis`].
    work_millis: i64,
}

/// What one lifecycle event came to: the `LifecycleEventStatus` it reached and the
/// diagnostics CodeDeploy reports with it.
#[derive(Debug, Clone)]
struct EventResult {
    /// A `LifecycleEventStatus`; see [`LIFECYCLE_EVENT_STATUS_SUCCEEDED`].
    status: &'static str,
    /// What the agent reports about the script of that event, where it ran one.
    diagnostics: Option<Diagnostics>,
}

impl EventResult {
    /// An event that completed with nothing to report: the agent's own step, or an
    /// event the revision declares no script for.
    fn succeeded() -> Self {
        Self {
            status: LIFECYCLE_EVENT_STATUS_SUCCEEDED,
            diagnostics: None,
        }
    }

    /// An event the agent never ran: there was nothing on the host for it to act
    /// on.
    fn skipped() -> Self {
        Self {
            status: LIFECYCLE_EVENT_STATUS_SKIPPED,
            diagnostics: None,
        }
    }

    /// An event that failed, carrying the `LifecycleErrorCode`, the script it is
    /// about and the message the agent reports for it.
    fn failed(error_code: &str, script_name: Option<&str>, message: String) -> Self {
        Self {
            status: LIFECYCLE_EVENT_STATUS_FAILED,
            diagnostics: Some(Diagnostics {
                error_code: Some(error_code.to_string()),
                script_name: script_name.map(str::to_string),
                message: Some(message),
                log_tail: None,
            }),
        }
    }

    /// Whether this event failed.
    fn is_failure(&self) -> bool {
        self.status == LIFECYCLE_EVENT_STATUS_FAILED
    }
}

impl Agent<'_> {
    /// Perform every lifecycle event of the sequence on this host, in order.
    ///
    /// A failed event ENDS the run: "if the deployment of a revision fails on an
    /// instance, the remaining deployment lifecycle events are skipped" — the
    /// events after it are reported `Skipped` and the instance target `Failed`. The
    /// one exception is the one the API models: a deployment created with
    /// `ignoreApplicationStopFailures` "continues to the next deployment lifecycle
    /// event" when ApplicationStop failed, and the event keeps the failure it
    /// reported.
    fn perform(&mut self, previous: Option<&InstanceArchive>) -> InstanceRun {
        let mut events: Vec<LifecycleEvent> = Vec::new();
        let mut installed: Vec<String> = Vec::new();
        let mut failed = false;
        let mut ended = false;
        for name in SERVER_LIFECYCLE_EVENTS {
            if ended {
                let at = self.work_millis;
                events.push(self.event(name, &EventResult::skipped(), at));
                continue;
            }
            // Where in the run this event began: the events of one host are performed in
            // order, so each one starts where the one before it ended.
            let began = self.work_millis;
            let result = match *name {
                LIFECYCLE_EVENT_APPLICATION_STOP => self.application_stop(previous),
                LIFECYCLE_EVENT_DOWNLOAD_BUNDLE => self.download_bundle(),
                LIFECYCLE_EVENT_INSTALL => {
                    let (result, paths) = self.install(previous);
                    installed = paths;
                    result
                }
                other => self.run_declared_scripts(other),
            };
            if result.is_failure() {
                failed = true;
                ended = !(*name == LIFECYCLE_EVENT_APPLICATION_STOP
                    && self.work.ignore_application_stop_failures);
            }
            events.push(self.event(name, &result, began));
        }
        InstanceRun {
            instance_id: self.instance_id.to_string(),
            status: if ended || (failed && !self.work.ignore_application_stop_failures) {
                TARGET_STATUS_FAILED
            } else {
                TARGET_STATUS_SUCCEEDED
            },
            events,
            installed,
            work_millis: self.work_millis,
        }
    }

    /// One lifecycle event as the modelled `LifecycleEvent` a target reports, having begun
    /// `began_offset_ms` into the run.
    ///
    /// The run began at the instant of the pass that performed it, and the events of it
    /// occupy that run in order: an event's window is from where the event before it ended
    /// to where its own scripts stated they ended ([`InstanceRun::work_millis`]), so an
    /// event whose scripts stated nothing begins and ends at the same instant. An event that
    /// was never run has no end.
    fn event(&self, name: &str, result: &EventResult, began_offset_ms: i64) -> LifecycleEvent {
        let began = self.began_ms.saturating_add(began_offset_ms);
        let ended = self.began_ms.saturating_add(self.work_millis);
        LifecycleEvent {
            lifecycle_event_name: Some(name.to_string()),
            diagnostics: result.diagnostics.clone(),
            start_time: Some(timestamp(began)),
            end_time: (result.status != LIFECYCLE_EVENT_STATUS_SKIPPED)
                .then(|| timestamp(ended)),
            status: Some(result.status.to_string()),
        }
    }

    /// ApplicationStop: the scripts of the revision the host already holds.
    ///
    /// "The AppSpec file and scripts used for the ApplicationStop … are the ones
    /// from the previously deployed revision", so they are read out of the archive
    /// that deployment left on the host. An instance no revision has been deployed
    /// to has no archive to run anything out of, so nothing of an old application is
    /// stopped on it — and the event still COMPLETED: there was nothing for it to
    /// do, which is what CodeDeploy reports it as on a first deployment, rather than
    /// an event something skipped.
    fn application_stop(&mut self, previous: Option<&InstanceArchive>) -> EventResult {
        let Some(previous) = previous else {
            return EventResult::succeeded();
        };
        let archive =
            archive_directory(&self.work.deployment_group_id, &previous.deployment_id);
        let Some(document) = APP_SPEC_NAMES.iter().find_map(|name| {
            self.store
                .read_in_instance_guest(self.instance_id, &format!("{archive}/{name}"))
        }) else {
            // The host no longer holds that revision's AppSpec, so there is nothing
            // of the old application to stop.
            return EventResult::succeeded();
        };
        let Some(declaration) = appspec::declared_server_in_app_spec(&document) else {
            return EventResult::succeeded();
        };
        let scripts: Vec<DeclaredScript> = declaration
            .scripts_for(LIFECYCLE_EVENT_APPLICATION_STOP)
            .into_iter()
            .cloned()
            .collect();
        self.run_scripts(LIFECYCLE_EVENT_APPLICATION_STOP, &archive, &scripts)
    }

    /// DownloadBundle: the agent puts the revision's bundle on the host, under its
    /// own deployment root, which is where every later event reads a script from.
    ///
    /// A deployment whose revision NO bundle was read for has nothing to put there.
    /// That is not a revision this emulator judged unreadable — one it located and
    /// could not read the AppSpec out of fails the deployment where the revision is
    /// resolved ([`super::s3_revision`]) — but one it was shown nothing about, which
    /// is the reading that module states for an object store it cannot ask: the
    /// event completes, and what the deployment can then do on the host is what an
    /// AppSpec declaring nothing describes.
    fn download_bundle(&mut self) -> EventResult {
        let archive = archive_directory(
            &self.work.deployment_group_id,
            &self.work.deployment_id,
        );
        let entries: Vec<(String, String)> = self
            .work
            .bundle
            .entries()
            .map(|(path, contents)| (format!("{archive}/{path}"), contents.to_string()))
            .collect();
        if entries.is_empty() {
            return EventResult::succeeded();
        }
        let expected = entries.len();
        match self.store.put_in_instance_guest(self.instance_id, &entries) {
            Some(written) if written.len() == expected => EventResult::succeeded(),
            // The host would not take the whole revision — its storage is bounded —
            // so the bundle is not there to install from.
            _ => EventResult::failed(
                LIFECYCLE_ERROR_CODE_UNKNOWN,
                None,
                BUNDLE_NOT_STORED_MESSAGE.to_string(),
            ),
        }
    }

    /// Install: the agent copies the files the AppSpec's `files` section declares to
    /// their destinations on the host.
    ///
    /// Answering the event's result and the paths the deployment installed.
    ///
    /// It CLEARS before it copies: the agent keeps a record of what the group's last
    /// deployment put on the host and takes those files off first, so a destination
    /// still held once that is done is a file no deployment of this group put there —
    /// which is exactly what `fileExistsBehavior` is about ("files that already exist
    /// in a deployment target location but weren't part of the previous successful
    /// deployment"). The deployment then fails having copied nothing (`DISALLOW`,
    /// which is also what a deployment that stated no behavior does), replaces the
    /// file (`OVERWRITE`), or keeps the one on the host, which "is kept and used as
    /// part of the new deployment" (`RETAIN`) and is therefore still one of this
    /// deployment's files.
    fn install(&mut self, previous: Option<&InstanceArchive>) -> (EventResult, Vec<String>) {
        let behavior = self.work.file_exists_behavior.clone();
        if let Some(previous) = previous {
            let cleared = previous.installed.clone();
            self.store
                .remove_in_instance_guest(self.instance_id, &cleared);
        }
        // What every `files` entry of the revision copies, resolved before anything is
        // written: a copy the revision cannot make fails the event with nothing
        // written at all, and the copies are held to what one Install resolves
        // ([`MAX_INSTALLED_FILES`]) as they are collected.
        let mut copies: Vec<(String, String)> = Vec::new();
        let mut resolved_bytes: usize = 0;
        for declared in self.work.declaration.files.clone() {
            let Some(resolved) = self.copies_of(&declared) else {
                return (
                    EventResult::failed(
                        LIFECYCLE_ERROR_CODE_UNKNOWN,
                        None,
                        missing_source_message(&declared.source),
                    ),
                    Vec::new(),
                );
            };
            for (destination, contents) in resolved {
                resolved_bytes = resolved_bytes
                    .saturating_add(destination.len())
                    .saturating_add(contents.len());
                if copies.len() >= MAX_INSTALLED_FILES || resolved_bytes > MAX_INSTALLED_BYTES {
                    // More than one Install of this emulator carries, which is more
                    // than the host's own storage would take either: nothing is
                    // written and the event fails saying so.
                    return (
                        EventResult::failed(
                            LIFECYCLE_ERROR_CODE_UNKNOWN,
                            None,
                            FILES_NOT_INSTALLED_MESSAGE.to_string(),
                        ),
                        Vec::new(),
                    );
                }
                copies.push((destination, contents));
            }
        }
        let mut writes: Vec<(String, String)> = Vec::new();
        let mut installed: Vec<String> = Vec::new();
        for (destination, contents) in copies {
            if self
                .store
                .instance_guest_holds(self.instance_id, &destination)
            {
                match behavior.as_str() {
                    FILE_EXISTS_BEHAVIOR_RETAIN => {
                        installed.push(destination);
                        continue;
                    }
                    FILE_EXISTS_BEHAVIOR_OVERWRITE => {}
                    // `DISALLOW`
                    // ([`super::deployment_summary::FILE_EXISTS_BEHAVIOR_DISALLOW`]),
                    // which is also the behavior of a deployment that stated none.
                    _ => {
                        return (
                            EventResult::failed(
                                LIFECYCLE_ERROR_CODE_UNKNOWN,
                                None,
                                file_exists_message(&destination),
                            ),
                            Vec::new(),
                        )
                    }
                }
            }
            installed.push(destination.clone());
            writes.push((destination, contents));
        }
        if writes.is_empty() {
            return (EventResult::succeeded(), installed);
        }
        let expected = writes.len();
        match self.store.put_in_instance_guest(self.instance_id, &writes) {
            Some(written) if written.len() == expected => (EventResult::succeeded(), installed),
            _ => (
                EventResult::failed(
                    LIFECYCLE_ERROR_CODE_UNKNOWN,
                    None,
                    FILES_NOT_INSTALLED_MESSAGE.to_string(),
                ),
                Vec::new(),
            ),
        }
    }

    /// What one `files` entry copies, as `(destination path, contents)`.
    ///
    /// "`source` … identifies a file or directory in the application revision" and
    /// "`destination` … identifies the location on the instance where the file …
    /// should be copied": a source that names an entry of the bundle copies that one
    /// file into the destination directory under its own name, and one that names a
    /// directory — the revision's root included — copies everything under it,
    /// keeping the paths it has inside the revision.
    ///
    /// `None` when the revision holds nothing at that source, which is a copy the
    /// deployment cannot make.
    fn copies_of(&self, declared: &super::appspec::DeclaredFile) -> Option<Vec<(String, String)>> {
        let destination = declared.destination.trim_end_matches('/').to_string();
        let joined = |relative: &str| format!("{destination}/{relative}");
        if let Some(contents) = self.work.bundle.entry(&declared.source) {
            let source = entry_path(&declared.source);
            let name = source.rsplit('/').next().unwrap_or(&source);
            return Some(vec![(joined(name), contents.to_string())]);
        }
        let under = self.work.bundle.entries_under(&declared.source);
        if under.is_empty() {
            return None;
        }
        Some(
            under
                .into_iter()
                .map(|(relative, contents)| (joined(&relative), contents))
                .collect(),
        )
    }

    /// The scripts THIS deployment's revision declares for `lifecycle_event_name`,
    /// run out of the archive this deployment put on the host.
    fn run_declared_scripts(&mut self, lifecycle_event_name: &str) -> EventResult {
        let archive = archive_directory(
            &self.work.deployment_group_id,
            &self.work.deployment_id,
        );
        let scripts: Vec<DeclaredScript> = self
            .work
            .declaration
            .scripts_for(lifecycle_event_name)
            .into_iter()
            .cloned()
            .collect();
        self.run_scripts(lifecycle_event_name, &archive, &scripts)
    }

    /// Run `scripts` for `lifecycle_event_name` out of the archive directory
    /// `archive`, in the order the AppSpec lists them, stopping at the first one
    /// that fails.
    ///
    /// A location the archive holds no file at is the `ScriptMissing` the model
    /// names, and a script that exited non-zero is `ScriptFailed`; an event whose
    /// revision declares no script completed with nothing to report.
    fn run_scripts(
        &mut self,
        lifecycle_event_name: &str,
        archive: &str,
        scripts: &[DeclaredScript],
    ) -> EventResult {
        let mut last: Option<Diagnostics> = None;
        for script in scripts {
            let path = format!("{archive}/{}", entry_path(&script.location));
            if self
                .store
                .read_in_instance_guest(self.instance_id, &path)
                .is_none()
            {
                return EventResult::failed(
                    LIFECYCLE_ERROR_CODE_SCRIPT_MISSING,
                    Some(&script.location),
                    script_missing_message(&path),
                );
            }
            let commands = hook_commands(&self.hook_environment(lifecycle_event_name), &path);
            let Some(run) = self.store.run_in_instance_guest_with(
                self.instance_id,
                &commands,
                None,
                &self.identity,
            ) else {
                // The guest went away between the check above and the run.
                return EventResult::failed(
                    LIFECYCLE_ERROR_CODE_UNKNOWN,
                    Some(&script.location),
                    HOST_UNREACHABLE_MESSAGE.to_string(),
                );
            };
            let log_tail = log_tail(&run.output, &run.errors);
            // What that script STATED it took is time the agent spent on this host, whether
            // the script then succeeded or failed: the deployment is held open for it
            // ([`InstanceRun::work_millis`]).
            self.spent(run.took);
            if run.status != 0 {
                return EventResult {
                    status: LIFECYCLE_EVENT_STATUS_FAILED,
                    diagnostics: Some(Diagnostics {
                        error_code: Some(LIFECYCLE_ERROR_CODE_SCRIPT_FAILED.to_string()),
                        script_name: Some(script.location.clone()),
                        message: Some(script_failed_message(&script.location, run.status)),
                        log_tail,
                    }),
                };
            }
            last = Some(Diagnostics {
                error_code: Some(LIFECYCLE_ERROR_CODE_SUCCESS.to_string()),
                script_name: Some(script.location.clone()),
                message: None,
                log_tail,
            });
        }
        EventResult {
            status: LIFECYCLE_EVENT_STATUS_SUCCEEDED,
            diagnostics: last,
        }
    }

    /// Spend `took` of this run: the time a hook's program stated it took, held to what one
    /// host's run may state in total ([`MAX_HOST_WORK_MILLIS`]).
    fn spent(&mut self, took: ::std::time::Duration) {
        let stated = i64::try_from(took.as_millis()).unwrap_or(i64::MAX);
        self.work_millis = self
            .work_millis
            .saturating_add(stated)
            .min(MAX_HOST_WORK_MILLIS);
    }

    /// The environment variables CodeDeploy makes available to a hook of
    /// `lifecycle_event_name`; see the module docs.
    fn hook_environment(&self, lifecycle_event_name: &str) -> Vec<(&'static str, String)> {
        vec![
            ("APPLICATION_NAME", self.work.application_name.clone()),
            ("DEPLOYMENT_ID", self.work.deployment_id.clone()),
            (
                "DEPLOYMENT_GROUP_NAME",
                self.work.deployment_group_name.clone(),
            ),
            (
                "DEPLOYMENT_GROUP_ID",
                self.work.deployment_group_id.clone(),
            ),
            ("LIFECYCLE_EVENT", lifecycle_event_name.to_string()),
        ]
    }
}

/// The commands one hook's run performs in the host's shell: the deployment's own
/// environment variables, and then the script itself.
///
/// The script is run the way a script left on a machine is run (`sh FILE`, which
/// the guest's session performs out of the machine's own tree), in the same session
/// as the exports above it, so a script that reads one of those variables reads the
/// value this deployment published.
#[must_use]
fn hook_commands(environment: &[(&'static str, String)], script_path: &str) -> Vec<String> {
    let mut commands: Vec<String> = environment
        .iter()
        .map(|(name, value)| format!("export {name}={}", quoted(value)))
        .collect();
    commands.push(format!("sh {}", quoted(script_path)));
    commands
}

/// One value as a shell word that carries it verbatim: single-quoted, with a
/// single quote inside it written the way a shell writes one.
fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The last of what a script wrote, as `Diagnostics$logTail` reports it — "the last portion
/// of the diagnostic log" — held to [`MAX_LOG_TAIL_BYTES`] from the END.
///
/// A script's log is what it wrote on BOTH streams: a hook that reports why it failed reports
/// it the way a program does, on standard error, and the agent captures the script's output to
/// its log rather than keeping one stream and dropping the other. The lines are the ones the
/// guest answered ([`::service_com_amazonaws_ec2_amazonec2::state::GuestRun`] keeps the two
/// apart, because the agent that delivers a script reports them separately), each stream in
/// the order the script wrote it.
fn log_tail(output: &[String], errors: &[String]) -> Option<String> {
    if output.is_empty() && errors.is_empty() {
        return None;
    }
    let whole = output
        .iter()
        .chain(errors.iter())
        .cloned()
        .collect::<Vec<String>>()
        .join("\n");
    if whole.len() <= MAX_LOG_TAIL_BYTES {
        return Some(whole);
    }
    let mut start = whole.len() - MAX_LOG_TAIL_BYTES;
    while start < whole.len() && !whole.is_char_boundary(start) {
        start += 1;
    }
    Some(whole[start..].to_string())
}

/// `Diagnostics$message` for a script the revision declares that its archive holds
/// no file at: the agent names the path it looked for it at, which is the location
/// the AppSpec gave it resolved inside the deployment's own archive.
#[must_use]
pub fn script_missing_message(path: &str) -> String {
    format!("Script does not exist at specified location: {path}")
}

/// `Diagnostics$message` for a script that ran and exited non-zero: the location
/// the AppSpec named it by, and the status it exited with.
#[must_use]
pub fn script_failed_message(location: &str, status: i32) -> String {
    format!("Script at specified location: {location} failed with exit code {status}")
}

/// `Diagnostics$message` of the Install that found a file of its own in the way,
/// which is what `DISALLOW` fails for: the destination the revision would have
/// written.
#[must_use]
pub fn file_exists_message(destination: &str) -> String {
    format!(
        "The deployment failed because a specified file already exists at this location: \
         {destination}"
    )
}

/// `Diagnostics$message` of an Install whose `files` entry names a source the
/// revision does not carry: there is nothing to copy, so the deployment cannot
/// report the file as installed.
#[must_use]
pub fn missing_source_message(source: &str) -> String {
    format!("The deployment failed because the revision does not contain the source: {source}")
}

/// `Diagnostics$message` of a DownloadBundle whose host would not keep the whole
/// revision: the bundle is not on the host, so nothing can be installed from it.
pub const BUNDLE_NOT_STORED_MESSAGE: &str =
    "The deployment failed because the revision could not be stored on the instance";

/// `Diagnostics$message` of an Install the host would not take the revision's files
/// for.
pub const FILES_NOT_INSTALLED_MESSAGE: &str =
    "The deployment failed because the revision's files could not be written on the instance";

/// `Diagnostics$message` of an event whose host stopped answering while the agent
/// was performing it.
pub const HOST_UNREACHABLE_MESSAGE: &str =
    "The deployment failed because the instance stopped responding during the deployment";
