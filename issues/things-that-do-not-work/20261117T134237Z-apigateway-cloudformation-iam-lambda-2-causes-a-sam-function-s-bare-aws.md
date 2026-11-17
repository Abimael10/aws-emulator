Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudformation__iam__lambda.cfn-sam-bare-policy-name-unresolved-managed-policy-fails-role-creation-and-rolls-back apigateway__cloudformation__iam__lambda.cfn-sam-bare-managed-policy-name-and-numeric-tag-literals-with-deployment-stage
Issue-Key: apigateway__cloudformation__iam__lambda.cfn-sam-bare-policy-name-unresolved-managed-policy-fails-role-creation-and-rolls-back|run#10|AssertionError|assert control_status == "CREATE_COMPLETE", (
Issue-Key: apigateway__cloudformation__iam__lambda.cfn-sam-bare-managed-policy-name-and-numeric-tag-literals-with-deployment-stage|run#12|AssertionError|assert status == "CREATE_COMPLETE", f"stack {stack_name!r} ended {status!r}, not CREATE_COMPLETE"
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# 2 causes: a SAM function's bare AWS managed-policy name in Policies never resolves so the stack rolls back, and that rollback itself ends ROLLBACK_FAILED

> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Two distinct root causes, both reached through CreateStack on an AWS::Serverless::Function whose `Policies` list carries a bare AWS managed-policy NAME.

Cause 1 of 2: a bare AWS managed-policy name in `Policies` is not resolved to its managed-policy ARN, so the generated execution role cannot be created and the stack never reaches CREATE_COMPLETE.
AWS: the SAM transform accepts three forms in `Policies` — an AWS managed-policy ARN, an AWS SAM policy-template name, and a bare AWS managed-policy NAME. A bare name that corresponds to a real AWS managed policy (e.g. `AmazonS3ReadOnlyAccess`) is expanded to `arn:<partition>:iam::aws:policy/<Name>` and attached to the generated role via `ManagedPolicyArns`, and the stack reaches CREATE_COMPLETE. (Smithy: cloudformation CreateStack/DescribeStacks; the SAM transform's `Policies` handling is documented behavior, not modelled — so: recorded behavior only for the expansion rule.)
Emulator: the stack with a bare, real managed-policy name ends `ROLLBACK_COMPLETE` instead of CREATE_COMPLETE — the name is never resolved, so the role creation (and with it the function) fails.
Why: every SAM template that uses the bare-name shorthand — a very common authoring style — fails to deploy, and a client gets a rolled-back stack with no function, no role and no API.
Fix: the SAM transform's `Policies` expansion for AWS::Serverless::Function (the bare-managed-policy-name branch, alongside the ARN and policy-template branches) and the IAM role resource it emits.
Traces: apigateway__cloudformation__iam__lambda.cfn-sam-bare-policy-name-unresolved-managed-policy-fails-role-creation-and-rolls-back

Cause 2 of 2: when that same template also carries numeric tag literals and a deployment stage, the stack's rollback does not complete — it ends ROLLBACK_FAILED.
AWS: cloudformation DescribeStacks `StackStatus` for a create that fails is `ROLLBACK_IN_PROGRESS` and then `ROLLBACK_COMPLETE`; `ROLLBACK_FAILED` is reserved for a rollback that could not delete or restore a resource, and AWS does not produce it for a template whose only failure is an unresolvable managed-policy name — every resource it created is deletable. (Smithy: cloudformation DescribeStacks output member `Stacks[].StackStatus`, enum StackStatus includes ROLLBACK_COMPLETE and ROLLBACK_FAILED.)
Emulator: the stack ends `ROLLBACK_FAILED`.
Why: ROLLBACK_FAILED is a terminal state a client cannot recover from with a plain DeleteStack — it demands ContinueUpdateRollback or manual cleanup. Reporting it for an ordinary create failure traps callers (and CI) in a state that needs operator intervention and leaks the resources the rollback should have removed.
Fix: the stack rollback executor in the cloudformation service — the resource-deletion pass that decides ROLLBACK_COMPLETE vs ROLLBACK_FAILED — and whatever resource delete it is failing on for this template.
Traces: apigateway__cloudformation__iam__lambda.cfn-sam-bare-managed-policy-name-and-numeric-tag-literals-with-deployment-stage

## Evidence
Cause 1 — expected: the bare name `AmazonS3ReadOnlyAccess` resolves and the stack reaches CREATE_COMPLETE. Actual, verbatim:
  AssertionError: control: 'AmazonS3ReadOnlyAccess' must resolve and the stack must succeed; got 'ROLLBACK_COMPLETE'
Cause 2 — expected StackStatus CREATE_COMPLETE, and in the worst case ROLLBACK_COMPLETE. Actual, verbatim:
  AssertionError: stack 'sim2real-xsvc-stk-<id>' ended 'ROLLBACK_FAILED', not CREATE_COMPLETE
(the stack name is a per-run value; the diverging status value is 'ROLLBACK_FAILED').

## Steps to Reproduce
Cause 1: CreateStack with `Transform: AWS::Serverless-2016-10-31` and an AWS::Serverless::Function whose `Policies: [AmazonS3ReadOnlyAccess]` is a bare managed-policy name. Wait for a terminal status. Observed ROLLBACK_COMPLETE; expected CREATE_COMPLETE with the generated role carrying arn:aws:iam::aws:policy/AmazonS3ReadOnlyAccess in ManagedPolicyArns.
Cause 2: CreateStack with the same bare-name template plus numeric tag literals and an AWS::Serverless::Api deployment stage. Wait for a terminal status. Observed ROLLBACK_FAILED; expected ROLLBACK_COMPLETE (and, once Cause 1 is fixed, CREATE_COMPLETE).
