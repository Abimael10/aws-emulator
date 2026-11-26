Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__iam__ssm.change-set-role-arn-on-update-is-kept-for-later-change-sets
Issue-Key: cloudformation__iam__ssm.change-set-role-arn-on-update-is-kept-for-later-change-sets|run#17|AssertionError|assert stack["StackStatus"] == "UPDATE_COMPLETE", f"role update: {stack['StackStatus']!r}"
Issue-Commit: 043cf7bbddd41f9e9269741e32e8c49c8ecdcab2

# cloudformation: an UPDATE change set carrying a RoleARN whose policy permits the declared write is rolled back (UPDATE_ROLLBACK_COMPLETE) instead of completing

> _Note: this issue was identified against the code base at commit `043cf7bbddd41f9e9269741e32e8c49c8ecdcab2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteChangeSet

## Description
One net-new root cause in CloudFormation's change-set execution path.

Cause 1 of 1: a change set executed under a stack RoleARN fails the resource write the role IS allowed, so the stack update rolls back instead of completing.

AWS: CreateChangeSet accepts RoleARN (Smithy: CreateChangeSet.RoleARN, @length min 20 max 2048) and CloudFormation performs every resource-provider call of that change set as that service role, then keeps the role on the stack for later operations (ExecuteChangeSet itself takes no role; Smithy: ExecuteChangeSet input has no RoleARN). When the role's policy DOES permit the declared write — here an inline policy allowing `ssm:*` on `arn:<partition>:ssm:<region>:<account>:parameter<prefix>/*` and an AWS::SSM::Parameter whose Name is under `<prefix>` — the executed change set must reach UPDATE_COMPLETE and DescribeStacks must report that role in RoleARN. The role is a permission boundary on the deploy, not a blanket denial.

Emulator: executing that change set ends the stack in UPDATE_ROLLBACK_COMPLETE. The replay shows the status only; the check retries this update for up to 120 s and retries ONLY while every CREATE_FAILED reason of the new parameter resource is the SSM parameter handler's own denial text `Resource handler returned message: "Access denied for operation 'PutParameter'." (RequestToken: <uuid>, HandlerErrorCode: AccessDenied)` (fixed text; the RequestToken is a per-run value), so the emulator either kept denying a write the role's policy allows past any propagation slack, or failed that resource for some other reason — in both cases the update that AWS completes is rolled back here.

Why: a least-privilege deploy role becomes unusable rather than scoped. Every update of a stack that has a RoleARN fails, the stack is left in UPDATE_ROLLBACK_COMPLETE, the declared parameter is never written, and a client cannot distinguish "the role is correctly scoped" from "the role is ignored/denied" — the positive half of RoleARN semantics (an in-policy write succeeds and the role is retained for later change sets) is unreachable.

Note: the open issue 20261102T091623Z-cloudformation-stack-change-set-rolearn-is-not-the.md lists this same check under its Cause 1, but it carries the OPPOSITE replay key (a write OUTSIDE the role's policy reaching UPDATE_COMPLETE because the role was never the acting principal). The failure reported here is the inverse assertion — an in-policy write now rolling the update back — and is a distinct cause from that issue's.

Fix: the change-set execution path's authorization of resource-provider calls under the stack's RoleARN — the IAM policy evaluation of `ssm:PutParameter` for an AWS::SSM::Parameter whose name matches the role policy's `parameter<prefix>/*` resource pattern, and the retention of RoleARN on the stack for subsequent change sets.

Traces: cloudformation__iam__ssm.change-set-role-arn-on-update-is-kept-for-later-change-sets.

## Evidence
Check cloudformation__iam__ssm.change-set-role-arn-on-update-is-kept-for-later-change-sets, mismatch:

  File "check_cloudformation__iam__ssm_change_set_role_arn_on_update_is_kept_for_later_change_sets.py", line 188, in run
    assert stack["StackStatus"] == "UPDATE_COMPLETE", f"role update: {stack['StackStatus']!r}"
  AssertionError: role update: 'UPDATE_ROLLBACK_COMPLETE'

Expected (real AWS): after ExecuteChangeSet of an UPDATE change set created with RoleARN=<role allowing ssm:* on parameter<prefix>/*> that adds an AWS::SSM::Parameter named <prefix>/b, DescribeStacks reports StackStatus=UPDATE_COMPLETE and RoleARN=<that role>.
Actual (emulator): StackStatus=UPDATE_ROLLBACK_COMPLETE, sustained across the check's 120 s retry window (the retry only continues while the resource's CREATE_FAILED reason is the handler's fixed AccessDenied text for 'PutParameter').

## Steps to Reproduce
1. IAM: create a role trusted by cloudformation.amazonaws.com and put an inline policy allowing Action `ssm:*` on `arn:<partition>:ssm:<region>:<account>:parameter<prefix>/*`.
2. CloudFormation: CreateStack (no RoleARN) with a template declaring one AWS::SSM::Parameter named `<prefix>/a`; wait for CREATE_COMPLETE (RoleARN absent).
3. CreateChangeSet(StackName=<stack id>, ChangeSetType=UPDATE, RoleARN=<role arn>) with a template that also declares a parameter named `<prefix>/b`; wait CREATE_COMPLETE, then ExecuteChangeSet.
4. Poll DescribeStacks: AWS reaches UPDATE_COMPLETE with RoleARN=<role arn>; the emulator reaches UPDATE_ROLLBACK_COMPLETE. Inspect DescribeStackEvents for the CREATE_FAILED reason of the `B` resource.
5. Or replay the check id in trace_ids.
