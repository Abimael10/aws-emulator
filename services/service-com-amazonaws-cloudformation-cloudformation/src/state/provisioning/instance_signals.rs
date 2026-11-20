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
//! at all for a stack that declares no `CreationPolicy`. Reading one configuration is bounded
//! too: at most a configuration's documented size is looked at
//! ([`MAX_BOOT_CONFIGURATION_BYTES`]) and the walk over it passes each byte once
//! ([`signal_invocations`]). The signals themselves land in the stack's own bounded log
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
                signalled_already: stack
                    .signals
                    .iter()
                    .filter(|signal| {
                        signal.logical_resource_id == barrier.logical_id()
                            && signal.status == crate::state::resource_signal_status::SUCCESS
                    })
                    .map(|signal| signal.unique_id.clone())
                    .collect(),
            })
        })
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

/// The `SignalResource` API as a command line spells it (`aws cloudformation
/// signal-resource`), which is the other way a booting instance signals its stack — the one
/// "for … signals from anywhere other than an Amazon EC2 instance", reached from an instance
/// that has the CLI rather than the helper scripts.
const SIGNAL_RESOURCE_COMMAND: &str = "signal-resource";

/// WHICH status the boot configuration asks the instance to signal `logical_id` with, or
/// `None` where it asks it for no signal on that resource at all.
///
/// A bootstrap runs more than one command — `cfn-init` before `cfn-signal` is the pattern
/// `CreationPolicy` itself documents — so what is read here is each INVOCATION of a
/// signalling command on its own ([`signal_invocations`]), and the options of one invocation
/// say nothing about another.
///
/// Both ways of signalling name the resource they signal — the helper script as
/// `--resource <LogicalId>`, the API's command as `--logical-resource-id <LogicalId>` — so an
/// invocation that names ANOTHER resource outright is not this gate's signal. One whose
/// argument is a value the instance builds at boot (`--resource ${LogicalId}`, `-r
/// $RESOURCE`) names no resource this read can resolve, and the instances are the gate
/// resource's own, which is what makes their signal its own.
///
/// "The status of the signal, which is either success or failure" (`SignalResource`), and an
/// invocation states which it asks for: the API's own command states it as
/// `--status <STATUS>`, and the helper script states it as the exit code it reports
/// (`cfn-signal -e 0` / `--exit-code 0` is a bootstrap that succeeded, a non-zero one its
/// failure) or as `--success <true|false>`. A configuration that invokes neither way of
/// signalling asks for no signal, one whose invocation for this resource states a failure asks
/// for that, and one that invokes it without stating a failure asks for the success of an
/// instance that came up — which is what the owning service reporting that instance serving
/// says happened.
///
/// The two ways of asking are AWS's own, and the options are read as the words those commands
/// publish — split as a shell splits them, so a quoted value and a continued line are read as
/// the one invocation they are — and a value a template builds at boot (`-e $?`) or the
/// bootstrap itself computes (`--status "${STATUS}"`, set by a test the script runs) states no
/// failure here: what such a bootstrap did is work this emulator did not run, and the instance
/// the owning service reports serving is one that came up.
fn asked_signal_status(boot_configuration: &str, logical_id: &str) -> Option<&'static str> {
    let asked = decoded(boot_configuration);
    let mut asked_failure = None;
    for invocation in signal_invocations(&asked) {
        // The WORDS of that invocation, split as a shell splits them: the quoting and the
        // escaping — a line continuation's own backslash among them — are the shell's rules,
        // so they are read by the maintained word splitter rather than by a reading of this
        // crate's. An option joined to its value by `=` is the pair it is, which is why that
        // separator is split on here; a command line the splitter will not read (an unclosed
        // quote) states no option this read can rely on, so nothing is taken from it.
        let Ok(words) = ::shell_words::split(invocation) else {
            continue;
        };
        let tokens: Vec<&str> = words
            .iter()
            .flat_map(|word| word.split('='))
            .filter(|token| !token.is_empty())
            .collect();
        if names_another_resource(&tokens, logical_id) {
            continue;
        }
        if asks_for_failure(&tokens) {
            asked_failure = Some(crate::state::resource_signal_status::FAILURE);
            continue;
        }
        return Some(crate::state::resource_signal_status::SUCCESS);
    }
    asked_failure
}

/// Every INVOCATION of a signalling command the boot configuration carries: the text from the
/// command to the end of the command line it is on, so the options read for one invocation are
/// its own.
///
/// A command line ends at a newline or at one of the separators a shell starts another command
/// at — and a newline a BACKSLASH escapes ends nothing: that is a line continuation, so the
/// command line goes on to the next line and the options written there are this invocation's
/// ([`command_line_end`]). A long CLI invocation is habitually written that way, and reading it
/// as the first line alone would read an invocation's whole option list as absent.
///
/// Bounded in both directions, the configuration being a value a caller stored: the text read
/// is at most one configuration's documented size ([`decoded`]), each step advances past the
/// END of the invocation it took — so no byte is scanned twice and the invocations cannot
/// overlap — and each invocation is a SLICE of that text rather than a copy of it.
fn signal_invocations(asked: &str) -> Vec<&str> {
    let mut invocations = Vec::new();
    let mut rest = asked;
    loop {
        let found = [SIGNAL_HELPER_SCRIPT, SIGNAL_RESOURCE_COMMAND]
            .iter()
            .filter_map(|command| rest.find(command))
            .min();
        let Some(at) = found else {
            return invocations;
        };
        let invocation = &rest[at..];
        let end = command_line_end(invocation);
        invocations.push(&invocation[..end]);
        rest = &invocation[end..];
    }
}

/// Where the command line that starts at the beginning of `invocation` ENDS: the first
/// separator a shell starts another command at that is not a line continuation.
///
/// A backslash immediately before a newline continues the command line, so the scan passes that
/// newline and goes on looking. Every other separator ends the line, and a line nothing ends is
/// the rest of the text. Each byte is looked at once, so the scan is bounded by the text it
/// walks.
fn command_line_end(invocation: &str) -> usize {
    const SEPARATORS: [char; 4] = ['\n', ';', '&', '|'];

    let mut from = 0;
    while let Some(found) = invocation[from..].find(SEPARATORS) {
        let at = from + found;
        let continued = invocation[at..].starts_with('\n') && invocation[..at].ends_with('\\');
        if !continued {
            return at;
        }
        from = at + 1;
    }
    invocation.len()
}

/// Whether this invocation names a resource OTHER than `logical_id`: an argument it states
/// outright, rather than one the instance builds at boot (which carries a substitution and so
/// names no resource this read can resolve).
fn names_another_resource(tokens: &[&str], logical_id: &str) -> bool {
    tokens.windows(2).any(|pair| match pair {
        [option, value] => {
            matches!(*option, "-r" | "--resource" | "--logical-resource-id")
                && !value.contains('$')
                && *value != logical_id
        }
        _ => false,
    })
}

/// Whether this invocation asks for the FAILURE of the bootstrap it reports: the status the
/// API's command states, or the exit code — or the success flag — the helper script does.
fn asks_for_failure(tokens: &[&str]) -> bool {
    tokens.windows(2).any(|pair| match pair {
        [option, value] => {
            (option.eq_ignore_ascii_case("--status")
                && value.eq_ignore_ascii_case(crate::state::resource_signal_status::FAILURE))
                || (matches!(*option, "-e" | "--exit-code")
                    && value.parse::<i64>().is_ok_and(|code| code != 0))
                || (matches!(*option, "-s" | "--success") && value.eq_ignore_ascii_case("false"))
        }
        _ => false,
    })
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
