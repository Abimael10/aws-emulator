Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecr.deploy-ecr-pull-through-cache-rule-duplicate-prefix-rejected
Issue-Key: cloudformation__ecr.deploy-ecr-pull-through-cache-rule-duplicate-prefix-rejected|run#19|AssertionError|assert stack_failed_reasons == [_STACK_REASON], (
Issue-Commit: 7a64cae84a9cfda7163325030ecd9d33dd5ead34

# cloudformation__ecr: a stack rolled back for a duplicate AWS::ECR::PullThroughCacheRule prefix records no stack-level CREATE_FAILED reason

> _Note: this issue was identified against the code base at commit `7a64cae84a9cfda7163325030ecd9d33dd5ead34`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: when a CreateStack fails pre-deployment name-conflict validation — here an AWS::ECR::PullThroughCacheRule whose EcrRepositoryPrefix is already registered — the emulator rolls the stack back but emits no stack-level CREATE_FAILED event carrying the validation-summary reason, so DescribeStackEvents yields nothing for the stack resource itself. Failing check: cloudformation__ecr.deploy-ecr-pull-through-cache-rule-duplicate-prefix-rejected.

AWS: recorded behavior only (CloudFormation stack events are not modeled by the Smithy shapes beyond `StackEvent$ResourceStatusReason`). Alongside the resource-level CREATE_FAILED event, AWS emits a CREATE_FAILED event for the stack resource itself (LogicalResourceId == the stack name) whose ResourceStatusReason is the fixed sentence `Validation failed with <n> error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation.` — the fixed text is that whole sentence; the only variable part is `<n>`, the number of failed validations (1 here). The same recording also expects one DescribeEvents VALIDATION_ERROR row of ResourceType `AWS::ECR::PullThroughCacheRule` with ValidationName NAME_CONFLICT_VALIDATION, ValidationStatus FAILED, ValidationFailureMode FAIL, ValidationPath `/Resources/Rule` and reason `Resource of type 'AWS::ECR::PullThroughCacheRule' with identifier '<prefix>' already exists.` (fixed text with the declared prefix substituted), no stack resources listed, and the pre-existing rule left byte-identical.

Emulator: the duplicate stack does reach ROLLBACK_COMPLETE (that assertion passes) and the control stack with a free prefix deploys and its rule is now visible to ecr:DescribePullThroughCacheRules — but the list of ResourceStatusReason values for events whose LogicalResourceId is the stack name and whose ResourceStatus is CREATE_FAILED is empty: `expected the stack's CREATE_FAILED reason to be 'Validation failed with 1 error(s). ...', got []`. The check aborts there, so the VALIDATION_ERROR row set, the empty ListStackResources result and the untouched-existing-rule legs after it were never reached and are not claimed here.

Why: a client that reads the stack-level failure reason to learn why a rollback happened — and to decide whether to call DescribeEvents for per-resource detail, which is exactly what the sentence instructs — sees no reason at all and reports the rollback as unexplained. Deploy tooling that surfaces StackStatusReason to users shows an empty cause.

Fix: the CloudFormation stack-event recorder for a failed create — it must emit the stack-level CREATE_FAILED event with the validation-summary reason whenever pre-deployment validation fails, not only resource-level events.

This is the same recorder named by Cause 4 of the open issue 20261109T141609Z-cloudformation-ecr-8-net-new-causes-two-aws.md (observed there via cloudformation__ecr.deploy-ecr-repository-duplicate-name-rejected, a duplicate AWS::ECR::Repository name). That issue does not carry this check's replay key: its Cause 6 for this check was the rule being invisible to ECR, which now works, and the check has advanced to the missing stack-level event. Closing the recorder gap should close both.

Traces: cloudformation__ecr.deploy-ecr-pull-through-cache-rule-duplicate-prefix-rejected (assertion at check_cloudformation__ecr_deploy_ecr_pull_through_cache_rule_duplicate_prefix_rejected.py line 201).

## Evidence
Suite: service `cloudformation__ecr` scored 5/10 (50%), 5 failed; 4 are owned by open issues, this one is not.

[mismatch] cloudformation__ecr.deploy-ecr-pull-through-cache-rule-duplicate-prefix-rejected
  check_...py line 201:
    events = stack_events(cfn, dup_id)
    stack_failed_reasons = [e.get("ResourceStatusReason", "") for e in events
                            if e.get("LogicalResourceId") == dup_stack and e.get("ResourceStatus") == "CREATE_FAILED"]
    assert stack_failed_reasons == [_STACK_REASON]
  AssertionError: expected the stack's CREATE_FAILED reason to be 'Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation.', got []

Expected (AWS): DescribeStackEvents contains exactly one event with LogicalResourceId == the stack name and ResourceStatus == CREATE_FAILED, whose ResourceStatusReason is the sentence above with the failed-validation count substituted.
Actual (emulator): no such event exists; the collected reasons are `[]`. The stack status itself is right (ROLLBACK_COMPLETE), and the prior legs of the check pass — the control stack deploys and ecr:DescribePullThroughCacheRules returns its rule with ecrRepositoryPrefix and upstreamRegistryUrl 'public.ecr.aws' — so this is specifically the stack-level event, not the rollback or the ECR write-through.
The stack names and prefixes of the recorded run are per-run values; the requirement is the event's presence and its fixed sentence.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. CreateStack a template whose only resource is `{"Type": "AWS::ECR::PullThroughCacheRule", "Properties": {"EcrRepositoryPrefix": <prefix>, "UpstreamRegistryUrl": "public.ecr.aws"}}` with a free prefix; wait for CREATE_COMPLETE (this registers the prefix).
3. CreateStack the identical template under a second stack name using that same, now-taken prefix; wait for the stack to settle (it reaches ROLLBACK_COMPLETE).
4. Call DescribeStackEvents on the second stack and select events where LogicalResourceId == the stack name and ResourceStatus == CREATE_FAILED, then read ResourceStatusReason.
   -> AWS: one such event, reason 'Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation.'
   -> Emulator: no such event; the selection is empty.
5. Re-replay check cloudformation__ecr.deploy-ecr-pull-through-cache-rule-duplicate-prefix-rejected to confirm the fix (it then continues into the VALIDATION_ERROR row, empty-resources and untouched-rule legs).
