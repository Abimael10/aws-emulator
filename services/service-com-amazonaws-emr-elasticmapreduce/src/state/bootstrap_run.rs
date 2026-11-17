// SPDX-License-Identifier: CC0-1.0

//! RUNNING a cluster's BOOTSTRAP ACTIONS: fetching each action's script out of Amazon S3 and
//! performing it before the cluster reports itself ready.
//!
//! # The rule this module implements
//!
//! "A list of bootstrap actions to run before Hadoop starts on the cluster nodes"
//! (`RunJobFlow$BootstrapActions` doc), each one "the script run by the bootstrap action" with
//! "[the] location in Amazon S3 of the script to run during a bootstrap action"
//! (`BootstrapActionConfig$ScriptBootstrapAction`, `ScriptBootstrapActionConfig$Path` docs). So
//! a launch that carried bootstrap actions does not merely RECORD them: the cluster occupies
//! `BOOTSTRAPPING` — the state between its capacity being provisioned and its being ready —
//! and in it each action's script is fetched from the location its `Path` names and PERFORMED,
//! in launch order, so the script's own effects are in place by the time the cluster reports
//! `RUNNING`/`WAITING` and a step of it can run. A cluster whose launch carried none is owed
//! nothing here and comes up as it always did.
//!
//! The outcome is the script's own: a script that exited zero is a bootstrap action that
//! succeeded, and one that exited anything else takes the CLUSTER down — it reaches
//! `TERMINATED_WITH_ERRORS` through `TERMINATING`, carrying the modelled
//! `BOOTSTRAP_FAILURE` reason ([`ClusterRecord::begin_bootstrap_failure`]) — rather than
//! leaving the cluster to come up as though the action had run. No later action of that
//! cluster runs: the nodes the actions were running on are going away.
//!
//! # What a script reaches
//!
//! The SAME bounded session a `command-runner.jar` step's command line reaches
//! ([`::service_common::posix_shell_session`], whose catalog of session-local utilities and
//! whose bounds are stated there), with the AWS CLI's Amazon S3 transfers as the one program
//! its environment provides ([`super::step_command_transfers`]) — really made against this
//! emulator's own object store, signed as the role the cluster's Amazon EC2 instances obtain
//! from their instance profile ([`super::instance_profile_credentials`]), which is the
//! credential a bootstrap action runs with. A script is performed as a SCRIPT rather than as
//! one command line, so the session is given its own working tree
//! ([`ShellSession::with_file_utilities`]) and with it the grammar's control flow and the file
//! utilities a script is written in. That tree is session-local memory, keyed by the path the
//! script named and bounded by that session: no host file, directory or path is opened,
//! created or read. NO host command, process or shell is started here or anywhere this calls,
//! and a command the session provides no program for answers the status a shell gives a
//! command it could not find — which is a non-zero status, so such a script fails its cluster
//! rather than being reported as a script that ran.
//!
//! "A list of command line arguments to pass to the bootstrap action script"
//! (`ScriptBootstrapActionConfig$Args` doc) is persisted and reported by
//! `ListBootstrapActions` ([`super::bootstrap_actions`]); the session resolves no POSITIONAL
//! parameter, so a script that reads one reads an empty value — the action still runs, and
//! widening that session is its own change rather than this one.
//!
//! # When a script runs, and what one call may spend
//!
//! An action is owed its run from the instant its cluster entered `BOOTSTRAPPING` — the phase
//! the clock owes a launch ([`ClusterRecord::advance_to`]) — and the run is performed by the
//! next operation that reports that cluster, with no store guard held, exactly as a step's
//! command is ([`super::command_step_run`]). A cluster whose actions have not all run does not
//! become ready ([`ClusterRecord::bootstrap_actions_have_run`]): its bootstrap actions run
//! "before Hadoop starts on the cluster nodes", so it stays `BOOTSTRAPPING` until each has
//! answered rather than reporting itself ready for a script nothing has performed.
//!
//! Where one runs — the thread, the claim that keeps one action performed once however many
//! callers report its cluster, and the permit — is [`super::performed_programs`]'s. What ONE
//! call spends is bounded here ([`MAX_BOOTSTRAP_ACTIONS_RUN_PER_CALL`],
//! [`BOOTSTRAP_CALL_CEILING`]); an action whose turn did not come stays owed its run and the
//! next call performs it.

use ::service_common::posix_shell_session::{
    EnvironmentPrograms, SessionConfig, ShellSession,
};

use super::*;

/// How large a bootstrap script this emulator reads.
///
/// CHOSEN, not an AWS limit: the script is an object a caller uploaded, so what one run takes
/// into this process is bounded here rather than by the object's own size. It is enforced by
/// the RANGED read, so nothing larger is ever buffered, and a script that is not text is one
/// no session can read.
pub const MAX_BOOTSTRAP_SCRIPT_BYTES: usize = 256 * 1024;

/// How many bootstrap actions ONE call performs, leaving the rest to the next call that
/// reports the cluster.
///
/// CHOSEN to be the ceiling Amazon EMR puts on one cluster's actions — "you can specify up to
/// 16 bootstrap actions per cluster" (Amazon EMR Management Guide, "Create bootstrap actions to
/// install additional software") — so a cluster whose launch carried the most actions it may
/// settles in one call rather than needing a second.
pub const MAX_BOOTSTRAP_ACTIONS_RUN_PER_CALL: usize = 16;

/// How long ONE call spends performing owed bootstrap actions before it leaves the rest to the
/// next call.
///
/// CHOSEN, the sibling of the step-command queue's own ceiling
/// ([`super::command_step_run::COMMAND_CALL_CEILING`]): the ceiling on what a REQUEST costs,
/// beside the ceiling on what one script's transfers cost. A further action is performed only
/// while a whole one still fits inside this span.
pub const BOOTSTRAP_CALL_CEILING: Duration = Duration::from_secs(8);

/// The thread one bootstrap action's script is performed on
/// ([`performed_on_its_own_thread`]).
const BOOTSTRAP_THREAD_NAME: &str = "emr-bootstrap-action";

/// What ONE bootstrap action of a cluster came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapActionAnswer {
    /// The script RAN: the status it exited with, which is what the action came to.
    Exited(i32),
    /// Its script was not fetched from the location the action names, so there was nothing to
    /// run; the detail is what the object store answered about it.
    Unfetched(String),
    /// The session performed nothing of the script at all, so nothing is known about what it
    /// would have come to.
    NotPerformed,
}

/// One bootstrap action this cluster has ANSWERED for, kept in launch order.
#[derive(Debug, Clone)]
pub struct BootstrapActionRun {
    /// Which action of the launch's list this is, counted from one — the number Amazon EMR
    /// names a bootstrap action by when it reports one failing.
    pub position: usize,
    /// Where its script was fetched from, as the launch wrote it.
    pub path: String,
    /// What it came to.
    pub answer: BootstrapActionAnswer,
}

impl BootstrapActionRun {
    /// Whether this action succeeded: its script ran and exited zero, which is what lets the
    /// cluster go on coming up.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.answer == BootstrapActionAnswer::Exited(0)
    }

    /// `ClusterStateChangeReason$Message` for the shutdown this action brings, naming the node
    /// it ran on where the cluster holds one.
    #[must_use]
    pub fn failure_message(&self, instance_id: Option<&str>) -> String {
        let position = self.position;
        match &self.answer {
            // RECORDED/published Amazon EMR wording for a bootstrap action that ran and
            // exited non-zero, which names the node it ran on and the action's position.
            BootstrapActionAnswer::Exited(_) => match instance_id {
                Some(id) => format!(
                    "On the master instance ({id}), bootstrap action {position} returned a \
                     non-zero return code"
                ),
                None => {
                    format!("Bootstrap action {position} returned a non-zero return code")
                }
            },
            // The same for an action whose script the node could not fetch.
            BootstrapActionAnswer::Unfetched(_) => match instance_id {
                Some(id) => format!(
                    "Master instance ({id}) failed attempting to download bootstrap action \
                     {position} file from S3"
                ),
                None => format!(
                    "Failed attempting to download bootstrap action {position} file from S3"
                ),
            },
            // This emulator's own, as the step path words a submission it performed nothing
            // of: never a status nothing produced.
            BootstrapActionAnswer::NotPerformed => format!(
                "Bootstrap action {position} did not run: this emulator performed nothing of \
                 its script."
            ),
        }
    }
}

/// One bootstrap action this service owes a run.
#[derive(Debug, Clone)]
struct BootstrapActionPlan {
    /// Its position in the launch's list, counted from one.
    position: usize,
    /// The Amazon S3 location of its script, as the launch wrote it.
    path: String,
}

impl ClusterRecord {
    /// Every bootstrap action of this cluster that is owed a run, in launch order.
    ///
    /// A cluster owes them only while it is BOOTSTRAPPING — before that its capacity is not up
    /// and after it the phase is over — and only while every action answered so far succeeded:
    /// one that failed is taking the cluster down, so the actions behind it never run.
    fn owed_bootstrap_actions(&self) -> Vec<BootstrapActionPlan> {
        if self.status.state != cluster_state::BOOTSTRAPPING {
            return Vec::new();
        }
        if self.bootstrap_runs.iter().any(|run| !run.succeeded()) {
            return Vec::new();
        }
        self.bootstrap_actions
            .iter()
            .enumerate()
            .skip(self.bootstrap_runs.len())
            .map(|(index, config)| BootstrapActionPlan {
                position: index + 1,
                path: config.script_bootstrap_action.path.clone(),
            })
            .collect()
    }

    /// Whether every bootstrap action this cluster's launch carried has run and succeeded,
    /// which is what lets the cluster report itself ready.
    #[must_use]
    pub fn bootstrap_actions_have_run(&self) -> bool {
        self.bootstrap_runs.len() >= self.bootstrap_actions.len()
            && self.bootstrap_runs.iter().all(BootstrapActionRun::succeeded)
    }

    /// The action that FAILED, once one has.
    fn failed_bootstrap_action(&self) -> Option<&BootstrapActionRun> {
        self.bootstrap_runs.iter().find(|run| !run.succeeded())
    }

    /// Record what one bootstrap action came to.
    ///
    /// Takes the answer only for the action whose turn it IS — the one after every action
    /// already answered for — so a run another call recorded in the window this one's guard was
    /// released is not recorded twice and no action is answered for out of order.
    pub(crate) fn record_bootstrap_action_run(&mut self, run: BootstrapActionRun) {
        if run.position == self.bootstrap_runs.len() + 1 {
            self.bootstrap_runs.push(run);
        }
    }

    /// Start the shutdown a FAILED bootstrap action brings, answering whether it was started.
    ///
    /// A bootstrap failure is a failure of the CLUSTER: its nodes could not be brought up the
    /// way the launch asked, so the cluster shuts down carrying the modelled reason for it
    /// (`ClusterStateChangeReasonCode` `BOOTSTRAP_FAILURE`) and the sentence Amazon EMR words
    /// that reason with, and the terminal state it reaches is `TERMINATED_WITH_ERRORS`
    /// ([`terminal_cluster_state`]) rather than `TERMINATED`.
    ///
    /// Answers `false` without touching the cluster when no action of it failed, when it is
    /// already shutting down or gone, and when the caller holds it under termination
    /// protection — which "prevents the Amazon EC2 instances from being terminated"
    /// (`Cluster$TerminationProtected` doc), so a cluster launched under it keeps the nodes a
    /// caller asked to keep rather than losing them to this shutdown.
    pub(crate) fn begin_bootstrap_failure(&mut self, now: SystemTime) -> bool {
        if self.status.state != cluster_state::BOOTSTRAPPING || self.termination_protection_holds()
        {
            return false;
        }
        let instance_id = self.master_ec2_instance_id().map(str::to_string);
        let Some(message) = self
            .failed_bootstrap_action()
            .map(|run| run.failure_message(instance_id.as_deref()))
        else {
            return false;
        };
        self.status.state = cluster_state::TERMINATING.to_string();
        self.status.state_change_reason = Some(StateChangeReasonRecord {
            code: cluster_state_change_reason_code::BOOTSTRAP_FAILURE.to_string(),
            message: Some(message),
        });
        self.status.bootstrapping_due_at = None;
        self.status.ready_due_at = None;
        let terminated_at = now + TRANSITION_SPAN;
        self.status.terminated_due_at = Some(terminated_at);
        // The cluster takes its step queue with it, exactly as a caller's own shutdown does
        // ([`ClusterRecord::begin_termination`]): no step stays `PENDING` for a cluster whose
        // nodes are going away.
        for step in &mut self.steps {
            step.stop_with_cluster(terminated_at);
        }
        true
    }

    /// The Amazon EC2 instance id of this cluster's primary node, where Amazon EC2 launched
    /// one for it.
    ///
    /// "Valid values are MASTER, CORE or TASK" (`InstanceGroupConfig$InstanceRole` doc) and the
    /// fleet vocabulary spells the primary role the same way (`InstanceFleetType`), so the
    /// primary node is an instance of whichever collection fills that role. It is named by the
    /// id Amazon EC2 minted for it (`Instance$Ec2InstanceId`), which is the node a caller sees
    /// everywhere else. A cluster whose instances this emulator did not launch holds none, and
    /// then nothing is named.
    #[must_use]
    pub fn master_ec2_instance_id(&self) -> Option<&str> {
        let primary_groups: Vec<&str> = self
            .groups
            .iter()
            .filter(|group| group.config.instance_role == instance_group_type::MASTER)
            .map(|group| group.id.as_str())
            .collect();
        let primary_fleets: Vec<&str> = self
            .fleets
            .iter()
            .filter(|fleet| fleet.config.instance_fleet_type == instance_group_type::MASTER)
            .map(|fleet| fleet.id.as_str())
            .collect();
        self.ec2_instances
            .iter()
            .find(|instance| {
                instance
                    .instance_group_id
                    .as_deref()
                    .is_some_and(|id| primary_groups.contains(&id))
                    || instance
                        .instance_fleet_id
                        .as_deref()
                        .is_some_and(|id| primary_fleets.contains(&id))
            })
            .map(|instance| instance.ec2_instance_id.as_str())
    }
}

/// Perform every bootstrap action the cluster `cluster_id` already owes, and record what each
/// came to.
///
/// Called by the operations that report a cluster, BEFORE they take this service's store: the
/// run was owed the instant the cluster entered `BOOTSTRAPPING`, whoever asks about it next,
/// and neither the Amazon S3 read nor the session that performs the script may happen under
/// this service's lock. The store is taken only to read the owed actions and to write what each
/// came to, and this performs nothing at all for a cluster id this account holds none under.
///
/// Every transition this leads to was owed whether or not anybody called — the cluster's
/// bootstrap phase began at an instant the clock decided — so a reporting operation may apply
/// it: what it adds is the action's own answer, which is what the cluster then comes up or goes
/// down on.
pub async fn run_due_bootstrap_actions<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    cluster_id: &str,
    now: SystemTime,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let (owed, job_flow_role): (Vec<BootstrapActionPlan>, String) = {
        let mut store = state.lock();
        // The phases the clock owes are applied first, so a cluster whose capacity is up is
        // BOOTSTRAPPING and its actions are among the owed ones.
        match store.cluster_mut(cluster_id, now) {
            Some(cluster) => (
                cluster.owed_bootstrap_actions(),
                cluster.job_flow_role.clone().unwrap_or_default(),
            ),
            None => (Vec::new(), String::new()),
        }
    };
    if owed.is_empty() {
        return;
    }
    // What the cluster's Amazon EC2 instances obtain from their instance profile, resolved once
    // for this call: the credential a bootstrap action's own calls present, which is nothing at
    // all where that profile carries no role those instances may assume.
    let credentials = cluster_step_credentials(ctx, region, &job_flow_role).await;
    // What this CALL may spend performing them: a further action is started only while a whole
    // one still fits, so the rest stay owed to the next call.
    let started = ::std::time::Instant::now();
    for plan in owed.into_iter().take(MAX_BOOTSTRAP_ACTIONS_RUN_PER_CALL) {
        if started.elapsed().saturating_add(TRANSFER_BUDGET) > BOOTSTRAP_CALL_CEILING {
            return;
        }
        // One action is performed at a time, so an action another call is still performing —
        // including one that call has already given up waiting for — is not performed a second
        // time: a script's transfers are real effects on the object store.
        let Some(claim) = claimed_program(&format!("{cluster_id}/bootstrap/{}", plan.position))
        else {
            continue;
        };
        let answer =
            performed_bootstrap_action(engine, ctx, region, &plan, &credentials, claim).await;
        let Some(answer) = answer else {
            // Nothing answered: the action stays owed its run, so a later call performs it
            // rather than this one recording an answer nothing produced.
            continue;
        };
        let failed = answer != BootstrapActionAnswer::Exited(0);
        {
            let mut store = state.lock();
            if let Some(cluster) = store.clusters.get_mut(cluster_id) {
                cluster.record_bootstrap_action_run(BootstrapActionRun {
                    position: plan.position,
                    path: plan.path.clone(),
                    answer,
                });
                // The cluster can now reach what that answer leads to — ready once every
                // action has succeeded, or the shutdown a failure brings.
                cluster.advance_to(now);
            }
        }
        if failed {
            // The cluster is going down: the nodes the actions behind this one would have run
            // on are going away with it.
            return;
        }
    }
}

/// Perform ONE owed bootstrap action and answer what it came to, or `None` when nothing
/// answered for it at all.
async fn performed_bootstrap_action<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    plan: &BootstrapActionPlan,
    credentials: &StepCredentials,
    claim: InFlight,
) -> Option<BootstrapActionAnswer> {
    let script = match read_bootstrap_script(ctx, region, &plan.path).await {
        Ok(script) => script,
        // There is nothing to run: the node could not fetch the script the action names, which
        // is a failure of the action itself.
        Err(detail) => return Some(BootstrapActionAnswer::Unfetched(detail)),
    };
    // The CLI this script's environment provides, built per action so each gets its own
    // transfer budget, and presenting on every call it makes what the cluster's instances
    // obtained from their instance profile.
    let cli = step_object_cli(
        engine,
        ctx,
        region,
        credentials,
        &format!("bootstrap-{}", plan.position),
    );
    performed_on_its_own_thread(BOOTSTRAP_THREAD_NAME, TRANSFER_BUDGET, claim, move || {
        performed_script(&script, cli)
    })
    .await
}

/// The script of one bootstrap action, read from the object its `Path` names.
///
/// The read is RANGED, so at most [`MAX_BOOTSTRAP_SCRIPT_BYTES`] plus the one byte that shows
/// the object is past the bound is ever buffered, and a file that is not text is one no session
/// can read. It is made over the framework's in-process callback, the same transport a client's
/// own Amazon S3 request takes, so the bytes that run are the bytes that were uploaded.
async fn read_bootstrap_script(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    path: &str,
) -> Result<String, String> {
    let Some(object) = s3_object(path) else {
        return Err(format!(
            "The path {path} names no object of Amazon S3 to run."
        ));
    };
    let read = ::service_common::s3_client::s3_client(ctx.callback(), region)
        .get_object()
        .bucket(&object.bucket)
        .key(&object.key)
        .range(format!("bytes=0-{MAX_BOOTSTRAP_SCRIPT_BYTES}"))
        .send()
        .await
        .map_err(|error| {
            format!(
                "the object store answered {:?}: {}",
                ::aws_smithy_types::error::metadata::ProvideErrorMetadata::code(&error),
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        })?;
    let body = read
        .body
        .collect()
        .await
        .map_err(|error| format!("the script did not arrive in full: {error}"))?
        .into_bytes();
    if body.len() > MAX_BOOTSTRAP_SCRIPT_BYTES {
        return Err(format!(
            "the script is past the {MAX_BOOTSTRAP_SCRIPT_BYTES} bytes one may hold"
        ));
    }
    String::from_utf8(body.to_vec()).map_err(|_| "the script is not text".to_string())
}

/// What the session came to for `script`.
///
/// The session holds the parse, the allow-list of session-local utilities and the one
/// environment program this module's header states, and nothing else: no process is started
/// here. It is given no variables either — a bootstrap action is handed no environment of this
/// service's or of the host's — and the credential the CLI's own calls are signed with is held
/// by that program rather than by the session. The one status that is not an answer about the
/// script is the one a shell gives a utility it could not find, which is what the session
/// answers for a script it performed nothing of — unless the script ENDED the session itself,
/// which is a status it really asked for.
fn performed_script(script: &str, cli: Option<StepObjectCli>) -> BootstrapActionAnswer {
    // A script is written against storage, so the session is given a working tree of its own:
    // its redirections leave files the CLI then carries, and the grammar's control flow and the
    // file utilities a script uses are commands it performs.
    let config = SessionConfig::default();
    let mut session = match cli {
        Some(cli) => ShellSession::with_programs(
            BTreeMap::new(),
            config,
            Box::new(cli) as Box<dyn EnvironmentPrograms>,
        ),
        None => ShellSession::new(BTreeMap::new(), config),
    }
    .with_file_utilities();
    let status = session.run(script);
    if status == config.statuses.could_not_run && session.exit_status().is_none() {
        return BootstrapActionAnswer::NotPerformed;
    }
    BootstrapActionAnswer::Exited(status)
}

/// The state a cluster whose shutdown is due reaches, which follows from WHY it shut down.
///
/// `ClusterStateChangeReasonCode` separates the shutdowns a cluster was asked for or finished
/// its work with — `USER_REQUEST` and `ALL_STEPS_COMPLETED` — from the ones that are a FAILURE
/// of the cluster itself: `BOOTSTRAP_FAILURE`, `INSTANCE_FAILURE`, `INSTANCE_FLEET_TIMEOUT`,
/// `STEP_FAILURE`, `VALIDATION_ERROR` and `INTERNAL_ERROR`. A cluster that went down on one of
/// those ends `TERMINATED_WITH_ERRORS` — "the cluster was terminated due to errors" — and every
/// other one ends `TERMINATED`.
#[must_use]
pub fn terminal_cluster_state(reason: Option<&StateChangeReasonRecord>) -> &'static str {
    let failed = reason.is_some_and(|reason| {
        CLUSTER_FAILURE_REASON_CODES.contains(&reason.code.as_str())
    });
    if failed {
        cluster_state::TERMINATED_WITH_ERRORS
    } else {
        cluster_state::TERMINATED
    }
}

/// The `ClusterStateChangeReasonCode` values that are a FAILURE of the cluster rather than a
/// shutdown it was asked for or finished its work with
/// (`smithy_query(query="shape", shape="ClusterStateChangeReasonCode")`). See
/// [`terminal_cluster_state`].
const CLUSTER_FAILURE_REASON_CODES: &[&str] = &[
    cluster_state_change_reason_code::BOOTSTRAP_FAILURE,
    "INSTANCE_FAILURE",
    "INSTANCE_FLEET_TIMEOUT",
    "STEP_FAILURE",
    "VALIDATION_ERROR",
    "INTERNAL_ERROR",
];
