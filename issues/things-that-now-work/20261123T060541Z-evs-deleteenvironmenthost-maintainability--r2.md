Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 84690972c7a63a9952a7abe9f5e5702ab42c29df

# terminate_host_instance hand-rolls the EC2 termination write that the EC2 crate already single-homes in begin_instance_termination

_Filed: 2026-11-23T06:05:40Z_
_Operation: DeleteEnvironmentHost | Class: maintainability_

> _Note: this issue was identified against the code base at commit `84690972c7a63a9952a7abe9f5e5702ab42c29df`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::ec2_resources::terminate_host_instance` writes the `shutting-down` record, the pending move to `terminated`, the `user_initiated_reason` transition reason and the `Client.UserInitiatedShutdown` state reason inline — the exact body the EC2 crate (already this crate's dependency, reached here as `ec2::`) exposes as the ONE public writer of a termination, `begin_instance_termination`, on the same state struct this function already locks and calls `instances.get_mut` / `insert_instance` / `mint_instance_id` on.

So the same rule — what a terminated instance's record looks like, and that a termination is idempotent for an instance already terminating — exists twice, and the copy here can drift from the writer that calls itself the only one. It already diverges silently where the two have more to do: the EC2 writer also records the Spot-Fleet `terminated` event and gives back Capacity Reservation capacity on the same idempotent arm, neither of which this copy does. Both are inert for an EVS host today (`launch_host_instance` stores `spot_instance_request_id: None` and `detail: ec2::InstanceDetail::default()`, so no capacity reservation), which is exactly why the duplication is invisible until EC2's rule changes.

Calling `store.begin_instance_termination(instance_id, SystemTime::now()).is_some_and(|previous| !previous.is_terminating())` produces the identical record and the identical `bool` this function returns today, so every response and every existing test stays unchanged.

## Evidence

This crate's copy — `services/service-com-amazonaws-evs-amazonelasticvmwareservice/src/state/ec2_resources.rs:682-685` claims equivalence:
```
/// written through EC2's own record exactly as that service's own termination writes it:
/// `shutting-down` with the move to `terminated` the clock owes it, and the terminated
/// record kept, so the instance is indistinguishable from one `TerminateInstances`
/// stopped.
```
and then writes it by hand at `ec2_resources.rs:700-720`:
```
    let mut store = state.lock();
    let Some(instance) = store.instances.get_mut(instance_id) else {
        return false;
    };
    if instance.state.is_terminating() {
        return false;
    }
    instance.state = ec2::InstanceStateName::ShuttingDown;
    instance.pending_transition = Some(ec2::PendingTransition::starting_now(
        ec2::InstanceStateName::Terminated,
    ));
    instance.state_transition_reason = Some(ec2::user_initiated_reason(
        ::std::time::SystemTime::now(),
    ));
    instance.state_reason = Some((
        "Client.UserInitiatedShutdown".to_string(),
        "Client.UserInitiatedShutdown: User initiated shutdown".to_string(),
    ));
    true
```

The writer that already owns the rule, in the crate this module already depends on (`use ::service_com_amazonaws_ec2_amazonec2::state as ec2;`, `src/state.rs:19`) — `services/service-com-amazonaws-ec2-amazonec2/src/state/instances_and_filters.rs:276-302`:
```
    /// Start the termination of the instance `instance_id` names, as of `at`, and answer the
    /// state it was in — `None` when this store holds no such instance.
    ///
    /// The ONE writer of a termination, so every path that terminates an instance (a caller
    /// naming it, or a Spot Fleet cancellation terminating what the fleet launched) leaves
    /// the same record behind: `shutting-down` with the clock owing `terminated`, and the
    /// reason EC2 records for a shutdown somebody asked for. A termination is IDEMPOTENT — an
    /// instance already on its way to, or at, `terminated` keeps the transition it has — so
    /// the state answered here is the one a caller is told it was in.
    pub fn begin_instance_termination(
        &mut self,
        instance_id: &str,
        at: SystemTime,
    ) -> Option<InstanceStateName> {
        let instance = self.instances.get_mut(instance_id)?;
        let previous = instance.state;
...
        if !previous.is_terminating() {
            instance.state = InstanceStateName::ShuttingDown;
            instance.pending_transition =
                Some(PendingTransition::starting_now(InstanceStateName::Terminated));
            instance.state_transition_reason = Some(user_initiated_reason(at));
```
with the two steps the copy omits at `instances_and_filters.rs:308-323` (Spot-Fleet event, Capacity Reservation release).

The only caller is `DeleteEnvironmentHost` — `src/ops/delete_environment_host.rs:303-305`:
```
        if let Some(instance_id) = terminated_instance {
            crate::state::ec2_resources::terminate_host_instance(&*self.engine, &instance_id);
        }
```
Tests that cover it and must stay green unchanged: `tests/all/host_deletion.rs:699-758` (`deleting_a_host_terminates_the_ec2_instance_it_ran_on`, asserting `shutting-down`, then `terminated`, then that a repeated delete leaves it `terminated`).

## Steps to Reproduce

1. Read `services/service-com-amazonaws-evs-amazonelasticvmwareservice/src/state/ec2_resources.rs:693-721`: the termination record is written field by field.
2. Read `services/service-com-amazonaws-ec2-amazonec2/src/state/instances_and_filters.rs:276-326`: the same write, documented as "The ONE writer of a termination, so every path that terminates an instance ... leaves the same record behind", public on the same state struct this module already locks.
3. Note that the EVS copy omits the Spot-Fleet event and the Capacity Reservation release that writer performs on the same idempotent arm, so the two bodies already differ.
4. Replace the inline write with `store.begin_instance_termination(instance_id, ::std::time::SystemTime::now()).is_some_and(|previous| !previous.is_terminating())` and confirm `tests/all/host_deletion.rs:699-758` still passes with identical responses.
