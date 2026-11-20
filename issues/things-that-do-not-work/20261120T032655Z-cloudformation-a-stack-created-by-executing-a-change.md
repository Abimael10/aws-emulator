Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.change-set-on-stack-failure-with-disable-rollback-rejected cloudformation__ssm.change-set-on-stack-failure-do-nothing-keeps-the-created-resource
Issue-Key: cloudformation.change-set-on-stack-failure-with-disable-rollback-rejected|run#19|AssertionError|assert stack.get("DisableRollback") is True, (
Issue-Key: cloudformation__ssm.change-set-on-stack-failure-do-nothing-keeps-the-created-resource|run#20|AssertionError|assert stack.get("DisableRollback") is True, stack.get("DisableRollback")
Issue-Commit: 77be7cbe62301a22a0d964901db82eb2eef71a33

# cloudformation: a stack created by executing a change set with OnStackFailure=DO_NOTHING reports DisableRollback=false in DescribeStacks (1 cause)

> _Note: this issue was identified against the code base at commit `77be7cbe62301a22a0d964901db82eb2eef71a33`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteChangeSet

## Description
One net-new root cause in the CloudFormation emulator's change-set execution path, grounded in two failing behavioral checks that fail on the same assertion. (The second check is listed by the suite under the `cloudformation__ssm` heading, but its only failing assertion is a CloudFormation DescribeStacks field; the SSM half of that check passes.)

=== Cause 1 of 1: the change set's OnStackFailure is never recorded as the stack's DisableRollback ===

AWS: Smithy (com-amazonaws-cloudformation-cloudformation): CreateChangeSet input carries the member OnStackFailure (enum OnStackFailure, values DELETE | DO_NOTHING | ROLLBACK). Its documentation states: "DO_NOTHING - if the stack creation fails, do nothing. This is equivalent to specifying true for the DisableRollback parameter to the ExecuteChangeSet API operation" and "ROLLBACK - ... This is equivalent to specifying false for the DisableRollback parameter"; it also states that when OnStackFailure is specified, ExecuteChangeSet's DisableRollback must not be. Smithy: ExecuteChangeSet input carries DisableRollback (boolean); DescribeStacks output shape Stack carries DisableRollback, documented "Boolean to enable or disable rollback on stack creation failures: true: disable rollback. false: enable rollback." So a stack whose change set was created with OnStackFailure=DO_NOTHING and then executed without DisableRollback must report DisableRollback=true on every later DescribeStacks — equally for a create that succeeds (CREATE_COMPLETE) and for one that fails and is deliberately left standing (CREATE_FAILED, no rollback events).

Emulator: ExecuteChangeSet applies the DO_NOTHING semantics but never records the flag. After ExecuteChangeSet (no DisableRollback in the request) of a change set created with OnStackFailure=DO_NOTHING, DescribeStacks returns the stack with DisableRollback=false. The replay reports, on the succeeding create: `AssertionError: OnStackFailure=DO_NOTHING must be recorded as DisableRollback=True, got False`; and on the failing create: `assert stack.get("DisableRollback") is True` -> `AssertionError: False`. Everything around it is already correct in both checks, which isolates the defect to the flag and not to the failure semantics: ExecuteChangeSet(DisableRollback=true) over such a change set is correctly refused with ValidationError "Either DisableRollback or OnStackFailure can be specified, but not both. Validation error as OnStackFailure parameter was specified during CreateChangeSet." and leaves the change set CREATE_COMPLETE/AVAILABLE with the stack REVIEW_IN_PROGRESS; DescribeChangeSet echoes OnStackFailure=DO_NOTHING; the plain execute reaches CREATE_COMPLETE; and in the failing-create check the stack settles in CREATE_FAILED with StackStatusReason "The following resource(s) failed to create: [<logical ids>]. " (fixed text plus the failing logical ids from the template), the already-created sibling resource is neither deleted nor rolled back, and it stays readable at its owning service.

Why: a client reads Stacks[0].DisableRollback to learn whether a failed stack was intentionally left standing for inspection or is going to be/was rolled back, and branches on it — whether to call RollbackStack/ContinueUpdateRollback, whether to delete and redeploy, whether a CREATE_FAILED stack is safe to inspect. Reading false where AWS reports true tells such a client that rollback is enabled for a stack that will never roll back, so it takes the wrong recovery path, and tooling that round-trips the flag back into a subsequent CreateStack/ExecuteChangeSet propagates the wrong failure policy.

Fix: the ExecuteChangeSet handler and the stack record it writes — the change set's stored OnStackFailure must be translated onto the stack when the change set is executed (DO_NOTHING -> DisableRollback true, ROLLBACK -> false, DELETE keeping its delete-on-failure behavior), and the DescribeStacks serializer must emit that stored value instead of a default false. Open issue 20261109T073558Z-cloudformation-4-net-new-behavioral-causes-deletechangeset-accepts names a different DisableRollback cause (CreateStack(DisableRollback=true) reaching CREATE_COMPLETE instead of CREATE_FAILED) and does not cover this one; the fixed issue 20261107T201046Z-cloudformation-3-net-new-causes-an-invalid-logs also does not (it concerns conflicting DisableRollback values being accepted, and here the conflicting-value refusal already passes).

Traces: cloudformation.change-set-on-stack-failure-with-disable-rollback-rejected; cloudformation__ssm.change-set-on-stack-failure-do-nothing-keeps-the-created-resource

## Evidence
cloudformation.change-set-on-stack-failure-with-disable-rollback-rejected [mismatch]
  expected: after CreateChangeSet(ChangeSetType=CREATE, OnStackFailure=DO_NOTHING) and a plain ExecuteChangeSet that reaches CREATE_COMPLETE, DescribeStacks reports Stack.DisableRollback = true.
  actual:   Stack.DisableRollback = false. AssertionError: OnStackFailure=DO_NOTHING must be recorded as DisableRollback=True, got False.
  (passing in the same check, so not part of this defect: DescribeChangeSet echoes OnStackFailure=DO_NOTHING; ExecuteChangeSet(DisableRollback=True) is refused with ValidationError "Either DisableRollback or OnStackFailure can be specified, but not both. Validation error as OnStackFailure parameter was specified during CreateChangeSet."; the refused execute leaves the change set CREATE_COMPLETE/AVAILABLE and the stack REVIEW_IN_PROGRESS; the plain execute then reaches CREATE_COMPLETE and EXECUTE_COMPLETE.)

cloudformation__ssm.change-set-on-stack-failure-do-nothing-keeps-the-created-resource [mismatch]
  expected: a change set created with OnStackFailure=DO_NOTHING whose execution fails leaves the stack CREATE_FAILED and reports Stack.DisableRollback = true.
  actual:   assert stack.get("DisableRollback") is True -> AssertionError: False.
  (passing in the same check: the stack settles in CREATE_FAILED; the failing resource's event reason is "<parameter name> already exists in stack <stack id>"; StackStatusReason is "The following resource(s) failed to create: [B]. "; the resource created before the failure is not deleted and SSM still reads its value.)

## Steps to Reproduce
Replay these behavioral checks against the emulator:

1. cloudformation.change-set-on-stack-failure-with-disable-rollback-rejected — CreateChangeSet(StackName=<new>, ChangeSetName=<new>, ChangeSetType=CREATE, TemplateBody=<one AWS::CloudFormation::WaitConditionHandle>, OnStackFailure="DO_NOTHING"); wait for CREATE_COMPLETE; call ExecuteChangeSet(ChangeSetName=<change set arn>, DisableRollback=True) and observe the (correct) ValidationError; then call ExecuteChangeSet(ChangeSetName=<change set arn>) with no DisableRollback, wait for the stack to reach CREATE_COMPLETE, and read DescribeStacks(StackName=<stack id>).Stacks[0].DisableRollback — it is false where AWS reports true.

2. cloudformation__ssm.change-set-on-stack-failure-do-nothing-keeps-the-created-resource — CreateChangeSet(ChangeSetType=CREATE, OnStackFailure="DO_NOTHING") for a template with two AWS::SSM::Parameter resources where the second collides with the name the first created; ExecuteChangeSet; wait until the stack leaves *_IN_PROGRESS (it correctly reaches CREATE_FAILED); read DescribeStacks(StackName=<stack id>).Stacks[0].DisableRollback — it is false where AWS reports true.
