Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecr__iam.deploy-ecr-pull-time-update-exclusion-duplicate-principal-rejected
Issue-Key: cloudformation__ecr__iam.deploy-ecr-pull-time-update-exclusion-duplicate-principal-rejected|run#24|AssertionError|assert stack_failed_reasons == [_STACK_REASON], (
Issue-Commit: 0ed36ac69b61f04777ef4cd85714b039c0593841

# cloudformation__ecr__iam: a stack rolled back for a duplicate AWS::ECR::PullTimeUpdateExclusion PrincipalArn records no stack-level CREATE_FAILED reason

> _Note: this issue was identified against the code base at commit `0ed36ac69b61f04777ef4cd85714b039c0593841`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: when a CreateStack fails pre-deployment name-conflict validation — here an AWS::ECR::PullTimeUpdateExclusion whose PrincipalArn is already excluded by another stack — the emulator rolls the stack back but emits no stack-level CREATE_FAILED event carrying the validation-summary reason, so DescribeStackEvents yields nothing for the stack resource itself. Failing check: cloudformation__ecr__iam.deploy-ecr-pull-time-update-exclusion-duplicate-principal-rejected.

AWS: recorded behavior only (CloudFormation stack events are not modeled beyond StackEvent$ResourceStatusReason). Alongside the resource-level CREATE_FAILED event, AWS emits a CREATE_FAILED event for the stack resource itself (LogicalResourceId == the stack name) whose ResourceStatusReason is the fixed sentence `Validation failed with <n> error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation.` — that whole sentence is fixed text; the only variable part is `<n>`, the count of failed validations (1 here). The same recording further expects one DescribeEvents VALIDATION_ERROR operation event for the offending logical id with ValidationName NAME_CONFLICT_VALIDATION, ValidationStatus FAILED, ValidationFailureMode FAIL, ValidationPath `/Resources/<logical id>` and reason `Resource of type 'AWS::ECR::PullTimeUpdateExclusion' with identifier '<principal arn>' already exists.` (fixed text with the request's principal ARN substituted), ListStackResources empty for the rejected stack, the pre-existing exclusion still listed by ecr:ListPullTimeUpdateExclusions, and after deleting the control stack the exclusion gone and iam:GetRole refusing NoSuchEntity `The role with name <name> cannot be found.` Those later legs were never reached and are not claimed here.

Emulator: the duplicate stack does reach ROLLBACK_COMPLETE (that assertion passes), and the control stack's role, its Ref-as-PrincipalArn output and its exclusion all read back correctly — but the list of ResourceStatusReason values for events whose LogicalResourceId is the stack name and whose ResourceStatus is CREATE_FAILED is empty: `AssertionError: expected the stack's CREATE_FAILED reason to be 'Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation.', got []`.

Why: a client that reads the stack-level failure reason to learn why a rollback happened — and to decide whether to call DescribeEvents for per-resource detail, which is exactly what the sentence instructs — sees no reason at all and reports the rollback as unexplained; deploy tooling that surfaces the stack status reason shows an empty cause.

Fix: the CloudFormation stack-event recorder for a failed create — it must emit the stack-level CREATE_FAILED event with the validation-summary reason whenever pre-deployment validation fails, not only resource-level events. This is the same recorder named by the open issue 20261124T024116Z-cloudformation-ecr-a-stack-rolled-back-for-a.md (observed there for a duplicate AWS::ECR::PullThroughCacheRule prefix) and by Cause 4 of 20261109T141609Z-cloudformation-ecr-8-net-new-causes-two-aws.md (duplicate AWS::ECR::Repository name). Neither of those issues carries this check's replay key, so this check is not covered by them; closing the recorder gap should close all three.

Traces: cloudformation__ecr__iam.deploy-ecr-pull-time-update-exclusion-duplicate-principal-rejected (assertion at check_cloudformation__ecr__iam_deploy_ecr_pull_time_update_exclusion_duplicate_principal_rejected.py line 271).

## Evidence
Suite: service `cloudformation__ecr__iam` scored 0/1 (0%), 1 failed; it is the only failing check in the whole run that is not already owned by an open issue.

[mismatch] cloudformation__ecr__iam.deploy-ecr-pull-time-update-exclusion-duplicate-principal-rejected
  check_...py line 271:
    events = stack_events(cfn, dup_id)
    stack_failed_reasons = [e.get("ResourceStatusReason", "") for e in events
                            if e.get("LogicalResourceId") == dup_stack and e.get("ResourceStatus") == "CREATE_FAILED"]
    assert stack_failed_reasons == [_STACK_REASON]
  AssertionError: expected the stack's CREATE_FAILED reason to be 'Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation.', got []

Expected (AWS): DescribeStackEvents contains exactly one event with LogicalResourceId == the stack name and ResourceStatus == CREATE_FAILED whose ResourceStatusReason is that sentence with the failed-validation count substituted.
Actual (emulator): no such event exists; the collected reasons are `[]`. The stack status itself is right (ROLLBACK_COMPLETE) and everything before this point passes — the control stack reaches CREATE_COMPLETE, Ref of the AWS::ECR::PullTimeUpdateExclusion equals the role ARN, iam:GetRole returns that ARN with the declared trust policy, and ecr:ListPullTimeUpdateExclusions contains the principal — so this is specifically the stack-level event, not the rollback, the IAM role or the ECR write-through.
The stack names, role name and account of the recorded run are per-run values; the requirement is the event's presence and its fixed sentence.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. CreateStack (Capabilities=[CAPABILITY_IAM]) a template with an AWS::IAM::Role trusting ecr.amazonaws.com and an AWS::ECR::PullTimeUpdateExclusion whose PrincipalArn is Fn::GetAtt of that role; wait for CREATE_COMPLETE and read the role ARN from the stack outputs (this registers the exclusion).
3. CreateStack a second stack whose only resource is an AWS::ECR::PullTimeUpdateExclusion with PrincipalArn set to that same, already-excluded role ARN; wait for the stack to settle (it reaches ROLLBACK_COMPLETE).
4. Call DescribeStackEvents on the second stack, select events where LogicalResourceId == the stack name and ResourceStatus == CREATE_FAILED, and read ResourceStatusReason.
   -> AWS: one such event, reason 'Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation.'
   -> Emulator: no such event; the selection is empty.
5. Re-replay check cloudformation__ecr__iam.deploy-ecr-pull-time-update-exclusion-duplicate-principal-rejected to confirm the fix (it then continues into the NAME_CONFLICT_VALIDATION DescribeEvents row, the empty ListStackResources leg, the surviving-exclusion leg and the delete/NoSuchEntity legs).
