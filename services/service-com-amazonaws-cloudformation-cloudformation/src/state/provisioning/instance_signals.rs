// SPDX-License-Identifier: CC0-1.0

//! The success signals the INSTANCES of a signalled resource send, which is what releases the
//! resource-signal wait of a resource whose signals come from the instances it consists of.
//!
//! ## The rule this module carries
//!
//! A `CreationPolicy`'s `ResourceSignal` is released by the signals the resource's own
//! instances send: "you can use the `cfn-signal` helper script … to signal" and
//! `SignalResource` "is useful in cases where you want to send signals from anywhere other
//! than an Amazon EC2 instance", so for the two types AWS documents that bootstrap on — an
//! Auto Scaling group, whose launched instances each signal, and an instance, which signals
//! for itself — the signals are sent by the Amazon EC2 instances the resource consists of,
//! each naming its own instance id as the signal's `UniqueId` ("when you signal Amazon EC2
//! instances or Auto Scaling groups, specify the instance ID that you are signaling as the
//! unique ID").
//!
//! So a stack whose group carries a `CreationPolicy` and whose instances are told to signal
//! it reaches its terminal status once those instances are up, rather than waiting out a
//! `Timeout` nothing was ever going to signal inside: the signals are recorded for the
//! instances the OWNING service reports serving, under the ids that service assigned them,
//! and the wait those signals satisfy is released exactly as a client's own
//! `SignalResource` call releases one ([`crate::ops::signal_resource`]).
//!
//! Three things make such a signal due, and all three are read rather than assumed:
//!
//! * the resource's declaration asks for the wait (its `CreationPolicy`'s `ResourceSignal`,
//!   which is what opened the barrier — [`crate::state::wait_conditions`]);
//! * the service that owns the resource reports the instance SERVING — in service in the
//!   group, or running for an instance of its own — so nothing is signalled for an instance
//!   that is not up; and
//! * that instance's own BOOT CONFIGURATION, as Amazon EC2 holds it, asks it to signal THIS
//!   resource of its stack, and asks for a SUCCESS ([`asked_signal_status`]) — which is what
//!   decides, on AWS too, whether an instance sends a signal at all and which status it
//!   sends. A resource whose instances are not told to signal, or whose instances are told to
//!   report the failure of their own bootstrap, is held to its `Timeout` and fails there,
//!   exactly as before.
//!
//! That last question is answered by PERFORMING the configuration rather than by reading it:
//! the configuration is a shell program, so it is performed in the one bounded, session-local
//! POSIX/bash session this workspace performs caller-supplied shell text in
//! ([`::service_common::posix_shell_session`]) with the two signalling commands as the only
//! programs its environment provides ([`SignallingPrograms`]), and what the instance asks for
//! is the invocation that program really made. A bootstrap decides the status it reports the
//! way a program does — a conditional over the exit code its own configuration states, a
//! variable it keeps the word in, a retry loop around the call — and all of that is the
//! grammar's own here, so a bootstrap that reports FAILURE through a value it computed asks
//! for that failure exactly as one that writes the word on the command line does. What the
//! bootstrap's own WORK did is not read: a command this environment provides no program for —
//! `cfn-init`, a package install, the fetch of the instance's own id — answers success,
//! because the service that owns the instance reporting it SERVING is what says that work came
//! out, so no status of such a command is read as a bootstrap that failed
//! ([`performed_signals`]).
//!
//! What this records was owed at the instant the owning service reported the instance
//! serving, whether or not anybody called: that instant is when the instance signalled. It
//! is driven from [`super::advance_stabilisations_in`] for the reason every other call there
//! is — the answers come from other services, which nothing under this store's lock can
//! reach.
//!
//! ## Bounds
//!
//! One pass asks the owning service for one open gate's instances once and reads the boot
//! configuration of each instance of that gate that has not signalled yet at most once,
//! stopping as soon as it has found the signals the gate still needs
//! ([`InstanceGate::needed`]) — so the calls are bounded by the instances the owning service
//! lets one group hold, and only while such a gate is open and unsatisfied, which is nothing
//! at all for a stack that declares no `CreationPolicy`. Performing one configuration is
//! bounded too: at most a configuration's documented size is performed
//! ([`MAX_BOOT_CONFIGURATION_BYTES`]), everything one performance may cost is the session's
//! own bound (how many commands it performs, how far it nests, what one word expands to —
//! [`::service_common::posix_shell_session::Bounds`]), the invocations it records are capped
//! ([`MAX_SIGNALLING_INVOCATIONS`]), and a `sleep` it performs states a time nothing waits
//! for. The signals themselves land in the stack's own bounded log
//! ([`crate::state::MAX_SIGNALS_PER_STACK`]).

use super::*;

/// The resource types whose `CreationPolicy` signals are sent by the Amazon EC2 instances the
/// resource consists of, and how the instances of one are read.
///
/// These are the types AWS documents the signalling bootstrap on: an Auto Scaling group,
/// whose instances each signal the group's logical id, and an `AWS::EC2::Instance`, which
/// signals for itself. Every other type's signals come from a client's own `SignalResource`
/// call, and nothing here is asked about them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signalling {
    /// An Auto Scaling group: the instances are the ones Amazon EC2 Auto Scaling reports in
    /// service in it.
    AutoScalingGroup,
    /// An `AWS::EC2::Instance`: the instance is the resource itself, once Amazon EC2 reports
    /// it running.
    Instance,
}

impl Signalling {
    /// Which kind of resource `resource_type` is, or `None` for a type whose signals do not
    /// come from instances of its own.
    fn of(resource_type: &str) -> Option<Self> {
        if resource_type == crate::state::template::AUTOSCALING_GROUP {
            return Some(Self::AutoScalingGroup);
        }
        if resource_type == ec2_instance::EC2_INSTANCE {
            return Some(Self::Instance);
        }
        None
    }
}

/// One resource-signal wait a stack's create is open at whose signals come from the
/// resource's own instances: what a signal names it by, and the resource the instances belong
/// to.
#[derive(Debug, Clone)]
struct InstanceGate {
    /// The logical id the signals name — the `LogicalResourceId` of a `SignalResource` call.
    logical_id: String,
    /// The resource's type, which is what its events report it as.
    resource_type: String,
    /// The resource the instances belong to, as the owning service addresses it.
    physical_resource_id: String,
    /// How the instances of that resource are read.
    signalling: Signalling,
    /// How many MORE success signals this wait needs: its `Count`, less the signals the stack
    /// has already recorded for the resource. Nothing beyond that number can release the wait,
    /// so it is what a pass stops collecting at.
    needed: usize,
    /// The instances whose signal this stack has ALREADY recorded for the resource, by the
    /// unique id each was recorded under — which is that instance's own id.
    ///
    /// They are not read again: the signal they sent is already counted, so a pass looks at
    /// the instances that have not signalled yet, and a gate whose instances come into service
    /// over several passes still reaches its `Count`.
    signalled_already: BTreeSet<String>,
}

/// Record the success signals the instances of this Region's signalled resources have sent,
/// and release the waits those signals satisfy.
///
/// Called by [`super::advance_stabilisations_in`] before the waits that have RUN OUT are
/// failed, so a gate its instances have signalled is released rather than failed in the same
/// pass.
pub(super) async fn settle_instance_signals(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    state: &::engine::ServiceState<crate::state::ComAmazonawsCloudformationCloudformationState>,
    region: &str,
) {
    // Which gates are open, read under a lock of its own: the reads below reach other
    // services' handlers, which take those services' locks, so a client reading THIS service
    // must not be blocked behind them.
    let open: Vec<(String, Vec<InstanceGate>)> = {
        let store = state.lock();
        store
            .stacks
            .iter()
            .filter(|stack| stack.region == region)
            .filter_map(|stack| {
                let gates = open_instance_gates(stack);
                (!gates.is_empty()).then(|| (stack.stack_id.clone(), gates))
            })
            .collect()
    };
    for (stack_id, gates) in open {
        for gate in gates {
            // The instances the resource consists of, as the service that OWNS it reports
            // them now. One unanswered read is no evidence about them — a service is allowed
            // to throttle or be briefly unreachable — so the wait simply holds and the next
            // pass asks again.
            //
            // BOXED, like every other call this module's walks make from inside a future a
            // deep chain holds: each of these reads carries another service's whole request
            // chain, and holding one of them inline would put its state in THIS future — which
            // the caller holds on the stack while the rest of the pass below it runs
            // ([`super::advance_stabilisations_in`]).
            let Ok(instances) = Box::pin(serving_instances(ctx, region, &gate)).await else {
                continue;
            };
            // ...and, per instance, whether its own boot configuration asks it to signal the
            // stack, and with WHICH status. An instance Amazon EC2 holds no boot configuration
            // for was told nothing, so it signals nothing.
            //
            // What the pass stops at is the number of signals it has FOUND, not the number of
            // instances it has looked at: a group may hold instances told different things,
            // and the ones that signal are not necessarily the first the owning service
            // reports. The walk is bounded either way — it reads each serving instance of one
            // gate at most once, and that set is bounded by the capacity the owning service
            // lets a group have.
            let mut signalled: Vec<(String, &'static str)> = Vec::new();
            let unsignalled = instances
                .into_iter()
                .filter(|instance_id| !gate.signalled_already.contains(instance_id));
            for instance_id in unsignalled {
                if signalled.len() >= gate.needed || asks_for_failure_of(&signalled) {
                    break;
                }
                let read = Box::pin(ec2_instance::boot_configuration(ctx, region, &instance_id));
                if let Ok(Some(boot)) = read.await {
                    // A configuration that asks for the FAILURE of its own bootstrap sends
                    // that failure: "a failure signal causes CloudFormation to immediately
                    // fail the stack creation or update" (`SignalResource$Status`), so one
                    // such instance is the whole of what this gate's pass has to find — the
                    // operation does not reach a terminal status it was headed for, and is
                    // rolled back ([`crate::state::StoredStack::record_resource_signal`]).
                    if let Some(status) = asked_signal_status(&boot, &gate.logical_id) {
                        signalled.push((instance_id, status));
                    }
                }
            }
            if signalled.is_empty() {
                continue;
            }
            let now = SystemTime::now();
            // What a FAILURE signal's failure leaves for the services that own the stack's
            // resources, performed with the store RELEASED exactly as every other call this
            // module makes.
            let owed = {
                let mut store = state.lock();
                let Some(stack) = store.resolve_mut(region, &stack_id) else {
                    continue;
                };
                let mut owed = None;
                for (instance_id, status) in &signalled {
                    // Keyed by the signalled resource and the signal's unique id, so an
                    // instance already counted is not counted twice however many passes read
                    // it serving ([`crate::state::StoredStack::record_signal`]), and recorded
                    // through the one rule every signal this service receives is held to: the
                    // signal is reported on the resource it was sent to, a wait its `Count` is
                    // now met by proceeds, and a FAILURE fails the operation at once.
                    let owes = stack.record_resource_signal(
                        &gate.logical_id,
                        instance_id,
                        status,
                        Some(&gate.resource_type),
                        now,
                    );
                    owed = owed.or(owes);
                }
                owed
            };
            // "ROLLBACK — Roll back the stack creation. This is the default": the resources a
            // failed CREATE had already made are removed again in the services that own them.
            // A failed UPDATE's revert is the one path every rollback this service owes goes
            // through ([`crate::state::settle_due_rollbacks`]), which the operation driving
            // this pass performs for itself.
            let Some(crate::state::wait_conditions::ExpiredWaitOwes::Removal {
                resources,
                held,
                needs,
                policies,
            }) = owed
            else {
                continue;
            };
            let deleted = Box::pin(unwind(
                ctx,
                region,
                &resources,
                &Teardown {
                    stack_id: &stack_id,
                    held: &held,
                    needs: &needs,
                    on_refusal: OnRefusal::Stop,
                    policies,
                    // The first walk of this rollback: nothing has been refused yet.
                    left_alone: NOTHING_LEFT_ALONE,
                },
            ))
            .await;
            let mut store = state.lock();
            let Some(stack) = store.resolve_mut(region, &stack_id) else {
                continue;
            };
            stack.schedule_resource_events(&deleted.transitions);
            stack.record_materialised_resources(deleted.resources);
            // A resource whose owning service accepted the delete and still holds it holds
            // this rollback open: it is not reported complete while a resource it removed is
            // still there.
            stack.record_stabilisations(&deleted.removing, SystemTime::now());
            if let Some(failure) = deleted.failure {
                // Reported once this teardown has asked about every resource it still can
                // ([`crate::state::StoredStack::hold_teardown_failure`]).
                if !stack.hold_teardown_failure(
                    &deleted.transitions,
                    &deleted.removing,
                    &deleted.refused,
                    crate::state::status::ROLLBACK_FAILED,
                    &failure,
                ) {
                    stack.fail_transition(
                        SystemTime::now(),
                        crate::state::status::ROLLBACK_FAILED,
                        failure,
                    );
                }
            }
        }
    }
}

/// Whether one of the signals a pass has found is a FAILURE: the operation is failed by it, so
/// nothing further is read for that gate.
fn asks_for_failure_of(signalled: &[(String, &'static str)]) -> bool {
    signalled
        .iter()
        .any(|(_, status)| *status == crate::state::resource_signal_status::FAILURE)
}

/// The gates of one stack whose signals come from the resource's own instances: the
/// unsatisfied resource-signal waits of the operation in flight, for a resource of a type whose
/// instances signal and that the operation has already given a physical id.
///
/// A `CreationPolicy` "is invoked during resource creation", so the waits read here are the
/// ones the operation that CREATES the resource is held at — a create, and an update that adds
/// the resource or REPLACES it, which is what opened the barrier
/// ([`crate::state::StoredStack::opening_barriers`]). Every other wait — one whose signals a
/// client sends — is left to that client's own `SignalResource` calls.
///
/// A BATCHED replacement whose policy asks the UPDATE to wait on signals is a gate of its own
/// ([`rolling_update_gates`]): the instances that signal it are the replacements the batch in
/// flight launched.
fn open_instance_gates(stack: &crate::state::StoredStack) -> Vec<InstanceGate> {
    if stack.pending.is_none()
        || !matches!(
            stack.status.as_str(),
            crate::state::status::CREATE_IN_PROGRESS | crate::state::status::UPDATE_IN_PROGRESS
        )
    {
        return Vec::new();
    }
    let resources = stack.current_resources();
    stack
        .wait_conditions
        .iter()
        .filter(|barrier| !stack.barrier_satisfied(barrier))
        .filter_map(|barrier| {
            let resource = resources
                .iter()
                .find(|resource| resource.logical_id == barrier.logical_id())?;
            if resource.physical_resource_id.is_empty() {
                return None;
            }
            Some(InstanceGate {
                logical_id: resource.logical_id.clone(),
                resource_type: resource.resource_type.clone(),
                physical_resource_id: resource.physical_resource_id.clone(),
                signalling: Signalling::of(&resource.resource_type)?,
                needed: barrier
                    .count()
                    .saturating_sub(stack.successful_signals(barrier.logical_id())),
                signalled_already: signalled_already(stack, barrier.logical_id()),
            })
        })
        .chain(rolling_update_gates(stack))
        .collect()
}

/// The gates the BATCHED replacements of the operation in flight are waiting at: one per Auto
/// Scaling group whose `AutoScalingRollingUpdate` asks the update to wait on signals
/// (`WaitOnResourceSignals`: "specifies whether the Auto Scaling group waits on signals from
/// new instances during an update") and whose batch in flight has instances to hear from.
///
/// The signals are the REPLACEMENTS' own, sent exactly as the signals of a group's create are
/// — each instance naming its own id — so they are collected by this one pass and counted by
/// the replacement itself ([`crate::state::rolling_update::RollingUpdate`]). The instances that
/// have already signalled are left unread, which is what keeps the instances the update has not
/// replaced yet — they signalled the operation that created them — from being read again.
fn rolling_update_gates(stack: &crate::state::StoredStack) -> Vec<InstanceGate> {
    stack
        .open_rolling_updates()
        .into_iter()
        .filter(|plan| plan.policy.wait_on_resource_signals && !plan.batch.is_empty())
        .filter_map(|plan| {
            Some(InstanceGate {
                signalling: Signalling::of(&plan.resource_type)?,
                // At most one signal per instance of the batch is outstanding, which is what
                // one pass of such a gate has to find.
                needed: plan.batch.len(),
                signalled_already: signalled_already(stack, &plan.logical_id),
                logical_id: plan.logical_id,
                resource_type: plan.resource_type,
                physical_resource_id: plan.physical_resource_id,
            })
        })
        .collect()
}

/// The unique ids the stack has already recorded a SUCCESS signal for the resource
/// `logical_id` under — which, for the signals an instance sends, are those instances' own
/// ids.
fn signalled_already(
    stack: &crate::state::StoredStack,
    logical_id: &str,
) -> BTreeSet<String> {
    stack
        .signals
        .iter()
        .filter(|signal| {
            signal.logical_resource_id == logical_id
                && signal.status == crate::state::resource_signal_status::SUCCESS
        })
        .map(|signal| signal.unique_id.clone())
        .collect()
}

/// The ids of the instances the gate's resource consists of that its owning service reports
/// SERVING right now.
///
/// An instance that is not up has not signalled: a group reports its serving instances as the
/// ones in service in it, and an instance of its own is serving once Amazon EC2 reports it
/// running — which is the same answer the stack holds that resource's own creation open until
/// ([`ec2_instance::readiness`]).
async fn serving_instances(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    gate: &InstanceGate,
) -> Result<Vec<String>, Refusal> {
    match gate.signalling {
        Signalling::AutoScalingGroup => {
            crate::state::provisioner::compute_fleet::in_service_instance_ids(
                ctx,
                region,
                &gate.physical_resource_id,
            )
            .await
        }
        Signalling::Instance => {
            match ec2_instance::readiness(ctx, region, &gate.physical_resource_id).await? {
                Readiness::Ready => Ok(vec![gate.physical_resource_id.clone()]),
                // Still coming up, or in a state no instance serves from — or a call that
                // service refused about it: it has signalled nothing.
                Readiness::Pending(_) | Readiness::Failed(_) | Readiness::Refused(_) => {
                    Ok(Vec::new())
                }
            }
        }
    }
}

/// The `cfn-signal` helper script, which is how AWS documents an instance signalling the
/// stack that created it: "you can use the `cfn-signal` helper script … to signal".
const SIGNAL_HELPER_SCRIPT: &str = "cfn-signal";

/// The AWS CLI, which is the other way a booting instance signals its stack — the one "for …
/// signals from anywhere other than an Amazon EC2 instance", reached from an instance that has
/// the CLI rather than the helper scripts.
const AWS_CLI_PROGRAM: &str = "aws";

/// The CLI's command GROUP and the command of it that signals a resource, as the CLI spells
/// them (`aws cloudformation signal-resource`).
const CLOUDFORMATION_COMMAND_GROUP: &str = "cloudformation";
const SIGNAL_RESOURCE_COMMAND: &str = "signal-resource";

/// The options the two signalling commands name the RESOURCE they signal with: the helper
/// script's `--resource <LogicalId>` and the API command's own
/// `signal-resource --logical-resource-id <LogicalId>`.
const RESOURCE_OPTIONS: &[&str] = &["--logical-resource-id", "--resource", "-r"];

/// The option the API's command states the signal's STATUS with
/// (`SignalResource$Status`: "the status of the signal, which is either success or failure").
const STATUS_OPTION: &[&str] = &["--status"];

/// The options the HELPER SCRIPT states the same thing with: the exit code the bootstrap
/// reported (`cfn-signal -e 0` / `--exit-code 0` is a bootstrap that succeeded, a non-zero one
/// its failure), and the success flag (`-s false`).
const EXIT_CODE_OPTIONS: &[&str] = &["--exit-code", "-e"];
const SUCCESS_OPTIONS: &[&str] = &["--success", "-s"];

/// The value [`SUCCESS_OPTIONS`] carries for a bootstrap that did NOT succeed.
const NOT_SUCCESSFUL: &str = "false";

/// How many signalling invocations one boot program's record holds.
///
/// The EMULATOR's number: what decides the gate is the FIRST invocation of the program that
/// signals the gate's resource ([`asked_signal_status`]), so nothing is read past a handful of
/// them, and how many a program may perform at all is already bounded by the session's own
/// command budget.
const MAX_SIGNALLING_INVOCATIONS: usize = 16;

/// What one invocation of a signalling command asked the stack for.
#[derive(Debug, Clone)]
struct AskedSignal {
    /// The resource the invocation named, and `None` for one that named none this read can
    /// resolve — an argument the bootstrap computed to nothing, or an option written with the
    /// next option after it instead of a value.
    resource: Option<String>,
    /// Whether it asked for the FAILURE of the bootstrap it reports.
    failing: bool,
}

/// The two programs an instance SIGNALS its stack with, as the environment of the session its
/// boot program is performed in provides them.
///
/// Neither of them reaches this service: what the stack does with a signal follows from the
/// instance the OWNING service reports serving ([`settle_instance_signals`] records it under
/// that instance's own id), so what these programs do is record the invocation the boot program
/// really made — the resource it named and whether it reported its bootstrap failed — and
/// answer the success a CLI answers for a call it made. They are the whole of what the
/// environment provides, so every other command the bootstrap names is one the session reports
/// it could not run.
#[derive(Debug, Clone, Default)]
struct SignallingPrograms(
    ::std::sync::Arc<::std::sync::Mutex<Vec<AskedSignal>>>,
);

impl SignallingPrograms {
    /// The invocations the boot program performed, in the order it performed them.
    fn performed(&self) -> Vec<AskedSignal> {
        self.0.lock().map(|held| held.clone()).unwrap_or_default()
    }

    /// Record one invocation, up to [`MAX_SIGNALLING_INVOCATIONS`] of them.
    fn record(&self, asked: AskedSignal) {
        if let Ok(mut held) = self.0.lock() {
            if held.len() < MAX_SIGNALLING_INVOCATIONS {
                held.push(asked);
            }
        }
    }
}

impl ::service_common::posix_shell_session::EnvironmentPrograms for SignallingPrograms {
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        _files: &mut dyn ::service_common::posix_shell_session::ProgramFiles,
    ) -> Option<::service_common::posix_shell_session::ProgramOutcome> {
        // The name a program is known by, whichever path the bootstrap invoked it through:
        // `cfn-signal` is documented under `/opt/aws/bin`.
        let known_as = ::service_common::posix_shell_session::command_base_name(name);
        let asked = match known_as {
            SIGNAL_HELPER_SCRIPT => asked_by_the_helper(arguments),
            AWS_CLI_PROGRAM => asked_by_the_api(arguments),
            _ => None,
        }?;
        self.record(asked);
        Some(::service_common::posix_shell_session::ProgramOutcome {
            status: ::service_common::posix_shell_session::Statuses::default().success,
            output: Vec::new(),
        })
    }
}

/// What a `cfn-signal` invocation asked for, or `None` for one this program performs nothing
/// of.
///
/// "The exit code … 0 means success" (`cfn-signal`), so a non-zero exit code is the bootstrap
/// reporting that it failed, and `--success false` states the same thing the other way; an
/// invocation stating neither reports the success of a bootstrap that ran. The exit code a
/// bootstrap reports is the status of the work BEFORE it (`cfn-signal -e $?`), and that work is
/// work this emulator did not run — which is why a command this environment provides no program
/// for answers the success a serving instance says it had
/// ([`performed_signals`]) rather than a shell's own "no such utility" status.
fn asked_by_the_helper(arguments: &[String]) -> Option<AskedSignal> {
    let mut words = arguments.iter();
    let mut asked = AskedSignal {
        resource: None,
        failing: false,
    };
    while let Some(word) = words.next() {
        if let Some(value) = option_value(word, &mut words, RESOURCE_OPTIONS) {
            // The LAST option that names one is the resource the invocation signals; one whose
            // value came to nothing names none, and leaves whatever was named before it.
            asked.resource = value.or(asked.resource);
            continue;
        }
        if let Some(value) = option_value(word, &mut words, EXIT_CODE_OPTIONS) {
            asked.failing |= value
                .is_some_and(|value| value.parse::<i64>().is_ok_and(|code| code != 0));
            continue;
        }
        if let Some(value) = option_value(word, &mut words, SUCCESS_OPTIONS) {
            asked.failing |= value.is_some_and(|value| value.eq_ignore_ascii_case(NOT_SUCCESSFUL));
        }
    }
    Some(asked)
}

/// What an `aws` invocation asked for, or `None` for one that is not `aws cloudformation
/// signal-resource` at all — every other command group and command of the CLI is an invocation
/// this program performs nothing of.
///
/// The two words naming the group and the command are read among the invocation's OPERANDS,
/// past the options and their values, because the CLI takes its own options before the group as
/// well as after the command.
fn asked_by_the_api(arguments: &[String]) -> Option<AskedSignal> {
    let mut words = arguments.iter();
    let mut asked = AskedSignal {
        resource: None,
        failing: false,
    };
    let mut operands: Vec<&str> = Vec::new();
    while let Some(word) = words.next() {
        if let Some(value) = option_value(word, &mut words, RESOURCE_OPTIONS) {
            asked.resource = value.or(asked.resource);
            continue;
        }
        if let Some(value) = option_value(word, &mut words, STATUS_OPTION) {
            asked.failing |= value.is_some_and(|value| {
                value.eq_ignore_ascii_case(crate::state::resource_signal_status::FAILURE)
            });
            continue;
        }
        if word.starts_with('-') {
            // Every other option of the CLI: its own value, where it takes one, is the word
            // after it, which is then not one of the operands read below.
            if words.clone().next().is_some_and(|next| !next.starts_with('-')) {
                words.next();
            }
            continue;
        }
        operands.push(word);
    }
    let signalling = operands.contains(&CLOUDFORMATION_COMMAND_GROUP)
        && operands.contains(&SIGNAL_RESOURCE_COMMAND);
    signalling.then_some(asked)
}

/// The value `word` carries for one of the options `names`, read the one way the programs an
/// environment provides read an option
/// ([`::service_common::posix_shell_session::program_option_value`]) — `None` when `word` is
/// not one of them.
///
/// `Some(None)` is an option written with no value after it, and so is one written with the
/// next OPTION after it: the invocation named nothing for it, which for the resource is "no
/// resource this read can resolve".
fn option_value<'words>(
    word: &str,
    words: &mut impl Iterator<Item = &'words String>,
    names: &[&str],
) -> Option<Option<String>> {
    let value = ::service_common::posix_shell_session::program_option_value(word, words, names)?;
    Some(
        value.filter(|value| !value.is_empty() && !value.starts_with('-')),
    )
}

/// WHICH status the boot configuration asks the instance to signal `logical_id` with, or
/// `None` where it asks it for no signal on that resource at all.
///
/// The configuration is a PROGRAM, so what it asks for is what PERFORMING it asks for: it is
/// performed in the one bounded, session-local POSIX/bash session this workspace performs
/// caller-supplied shell text in ([`::service_common::posix_shell_session`], whose module docs
/// state its catalog and its bounds), with the two signalling commands as the programs the
/// environment provides ([`SignallingPrograms`]). So the conditional that picks the status a
/// bootstrap reports, the variable it keeps it in, the loop it retries the call from and the
/// quoting of the option's value are the GRAMMAR's rather than something scanned out of the
/// text — a bootstrap that reports its failure through a variable it set itself asks for that
/// failure, exactly as one that writes the word on the command line does. Nothing of it reaches
/// a process, a host path or the network: a command outside the session's catalog and these two
/// programs is one this environment provides no program for, and the program's own `sleep`
/// states a time nothing waits for.
///
/// Both ways of signalling name the resource they signal — the helper script as
/// `--resource <LogicalId>`, the API's command as `--logical-resource-id <LogicalId>` — so an
/// invocation that names ANOTHER resource is not this gate's signal. One whose argument came to
/// nothing names no resource this read can resolve, and the instances are the gate resource's
/// own, which is what makes their signal its own.
///
/// "The status of the signal, which is either success or failure" (`SignalResource`), and an
/// invocation states which it asks for ([`asked_by_the_api`], [`asked_by_the_helper`]). A
/// configuration that performs neither way of signalling asks for no signal, one whose
/// invocation for this resource states a failure asks for that, and one that invokes it without
/// stating a failure asks for the success of an instance that came up — which is what the
/// owning service reporting that instance serving says happened.
fn asked_signal_status(boot_configuration: &str, logical_id: &str) -> Option<&'static str> {
    let mut asked_failure = None;
    for asked in performed_signals(&decoded(boot_configuration)) {
        if asked
            .resource
            .as_deref()
            .is_some_and(|named| named != logical_id)
        {
            continue;
        }
        if asked.failing {
            asked_failure = Some(crate::state::resource_signal_status::FAILURE);
            continue;
        }
        return Some(crate::state::resource_signal_status::SUCCESS);
    }
    asked_failure
}

/// The signalling invocations PERFORMING the boot program `asked` really made.
///
/// The session is given the file utilities, because a boot program runs on a machine that has
/// storage of its own — which is also what makes the conditionals and the loops such a program
/// is written with forms this session performs
/// ([`::service_common::posix_shell_session::ShellSession::with_file_utilities`]) — and no
/// variables, because the facts of the machine are the owning service's and nothing here reads
/// one. Everything the performance may cost is the session's own bound, and the program is at
/// most one configuration's documented size ([`decoded`]).
///
/// A command this environment provides NO PROGRAM FOR answers the session's own SUCCESS rather
/// than a shell's "the utility could not be found". What this reading is after is the signal
/// alone: the bootstrap's own work — `cfn-init`, the packages it installs, the fetch of the
/// instance's own id — is work this emulator did not run, and the service that owns the
/// instance reporting it SERVING is what says that work came out. So the status of such a
/// command is not evidence of a bootstrap that failed: taking the shell's not-found status for
/// it would make the exit code the canonical bootstrap reports (`cfn-init …` then `cfn-signal
/// -e $?`, the pattern `CreationPolicy` documents) the status of a command nothing performed,
/// and would report a FAILURE no engine produced — while `cfn-init && cfn-signal -e 0` and a
/// script run under `set -e` would never reach their signalling line at all. A line the session
/// cannot PARSE keeps the shell's own syntax status: that is the configuration's own statement
/// rather than this reading's.
fn performed_signals(asked: &str) -> Vec<AskedSignal> {
    let statuses = ::service_common::posix_shell_session::Statuses::default();
    let programs = SignallingPrograms::default();
    let mut session = ::service_common::posix_shell_session::ShellSession::with_programs(
        ::std::collections::BTreeMap::new(),
        ::service_common::posix_shell_session::SessionConfig {
            statuses: ::service_common::posix_shell_session::Statuses {
                could_not_run: statuses.success,
                ..statuses
            },
            ..::service_common::posix_shell_session::SessionConfig::default()
        },
        Box::new(programs.clone()),
    )
    .with_file_utilities();
    // The configuration is ONE program, read by the grammar in one piece: that is what makes a
    // construct spanning lines the program's own and a variable one line sets there for the
    // next.
    session.run(asked);
    programs.performed()
}

/// The most of a boot configuration this read looks at: "the maximum size of the user data is
/// 16 KB" (`RunInstances$UserData`), so a configuration an instance really booted with is at
/// most that, and what a caller stored past it is not read here — the work one of these reads
/// costs is bounded whatever the stored value's size.
const MAX_BOOT_CONFIGURATION_BYTES: usize = 16 * 1024;

/// The boot configuration as TEXT, bounded at [`MAX_BOOT_CONFIGURATION_BYTES`]: "the user data
/// … is base64-encoded" on the wire (`Instance$UserData`), so the stored value is decoded —
/// and one the codec does not admit, or that is not text at all, is read as it was written
/// rather than dropped.
fn decoded(boot_configuration: &str) -> String {
    let mut text = ::aws_smithy_types::base64::decode(boot_configuration)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| boot_configuration.to_string());
    if text.len() > MAX_BOOT_CONFIGURATION_BYTES {
        // Cut at a character boundary at or below the bound, so the text stays the text it is
        // (a configuration of that size carries nothing this read is looking for anyway).
        let cut = (0..=MAX_BOOT_CONFIGURATION_BYTES)
            .rev()
            .find(|at| text.is_char_boundary(*at))
            .unwrap_or(0);
        text.truncate(cut);
    }
    text
}
