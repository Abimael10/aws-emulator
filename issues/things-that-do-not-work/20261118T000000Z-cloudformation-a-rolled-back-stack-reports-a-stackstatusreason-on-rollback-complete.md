Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.stack-rollback-events-golden-failed-and-cancelled
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.stack-rollback-events-golden-failed-and-cancelled|run#15|AssertionError|assert "StackStatusReason" not in final, f"ROLLBACK_COMPLETE carries a reason: {final.get('StackStatusReason')!r}"
Issue-Commit: ed3be584f32f7126fe01624e5a72b355a97afc86

# cloudformation: a stack that finished rolling a failed create back reports a StackStatusReason on its terminal ROLLBACK_COMPLETE, where AWS reports none

> _Note: this issue was split off the fix of
> `20261105T132157Z-cloudformation-8-net-new-root-causes-in-the-684719.md`, whose cause 6 named it
> in passing. That fix closed the three EVENT-level defects cause 6 is about (the extra stack-level
> `CREATE_FAILED`, the `ROLLBACK_IN_PROGRESS` reason copied from one resource instead of the
> aggregate, and the reason on the `ROLLBACK_COMPLETE` event). What is left is the STACK's own
> field, which is a separate surface and is pinned deliberately by many suites of the
> CloudFormation crate, so it needs a change of its own._

## Operation
DescribeStacks

## Description
Cause 1 of 1: `Stack$StackStatusReason` is reported on a stack resting in the terminal
`ROLLBACK_COMPLETE` status.

AWS: recorded behavior only (`Stack$StackStatusReason` is "success/failure message associated with
the stack status", a free string). A stack CloudFormation has FINISHED rolling back carries no
status reason at all: `DescribeStacks` answers a `ROLLBACK_COMPLETE` stack with no
`StackStatusReason` member. The failure that caused the rollback is reported where it happened —
on the failed resource's own `StackEvent$ResourceStatusReason`, and on the stack-level
`ROLLBACK_IN_PROGRESS` event, whose aggregate (`The following resource(s) failed to create: [...].
Rollback requested by user.`) names every resource the operation could not carry through.

Emulator: `DescribeStacks` on a stack whose create failed and rolled back answers
`StackStatusReason` `The following resource(s) failed to create: [Fast, Slow]. ` — the summary the
`ROLLBACK_IN_PROGRESS` transition carried, kept on the terminal status.

Why: a client polling `DescribeStacks` for a terminal status reads the reason member to tell a
rollback that itself went wrong (`ROLLBACK_FAILED`, which does carry one) from a rollback that
completed; a reason on the completed status makes the two indistinguishable, and a client that
surfaces whatever reason the terminal stack carries reports a failure for an operation that
finished exactly as `OnFailure=ROLLBACK` asked.

Fix: the pending terminal transition `crate::state::StoredStack::begin_rollback` opens
(`services/service-com-amazonaws-cloudformation-cloudformation/src/state/stack.rs`), whose
`PendingStatus::status_reason` must be `None` for `ROLLBACK_COMPLETE`. NOTE for the fixer: this is
a BROAD change, not a one-line one. Roughly three dozen suites of that crate assert the settled
stack's reason after a rolled-back create (`ssm_parameter_types`, `unresolvable_imports`,
`failed_create_rollback`, `stack_failure_reason`, `expired_wait_rollback`, `wait_conditions`,
`provisioned_resources`, `dynamic_references`, `ec2_typed_parameters`, … — `cargo test -p
service-com-amazonaws-cloudformation-cloudformation` names them all), and each of those
expectations has to move from the stack's own field to the event that really carries the failure.
Decide too whether the same holds for `UPDATE_ROLLBACK_COMPLETE`, which this issue carries no
recorded evidence about.

Traces: cloudformation.stack-rollback-events-golden-failed-and-cancelled.

## Evidence
cloudformation.stack-rollback-events-golden-failed-and-cancelled [mismatch]:
  AssertionError: ROLLBACK_COMPLETE carries a reason: 'The following resource(s) failed to create:
  [Fast, Slow]. '
  Expected: the `DescribeStacks` answer for the settled stack carries no `StackStatusReason`
  member at all. The logical ids inside the summary are that run's template's; what the assertion
  is about is the presence of the member, not its text.

The same check's stack-level EVENT rows already match AWS: `CREATE_IN_PROGRESS`/`User Initiated`,
`ROLLBACK_IN_PROGRESS` with the aggregate reason, `ROLLBACK_COMPLETE` with no reason.

## Steps to Reproduce
1. CreateStack a template with two `AWS::CloudFormation::WaitCondition` resources sharing one
   handle, one with `Timeout` 1 and one with a long one, so the create fails and rolls back.
2. Poll `DescribeStacks` to the terminal status (`ROLLBACK_COMPLETE`).
3. Read the answered `Stack`: it carries a `StackStatusReason`; AWS carries none.
4. Re-replay: cloudformation.stack-rollback-events-golden-failed-and-cancelled.
