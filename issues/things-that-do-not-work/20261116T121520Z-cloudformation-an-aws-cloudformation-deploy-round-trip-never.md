Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.redeploy-with-changed-template-applies-the-change cloudformation.redeploy-broken-importvalue-rolls-back-preserving-original
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.redeploy-with-changed-template-applies-the-change|_deploy#15|Timeout|result = subprocess.run(
Issue-Key: cloudformation.redeploy-broken-importvalue-rolls-back-preserving-original|_deploy_allow_failure#16|Timeout|result = subprocess.run(
Issue-Commit: 13e968f2d712b86cbfec63dd795b2bc21b7f9b4c

# CloudFormation: an `aws cloudformation deploy` round trip never completes — the change-set create/execute deploy of a stack serves no terminal outcome and the checks are killed at their budget

> _Note: this issue was identified against the code base at commit `13e968f2d712b86cbfec63dd795b2bc21b7f9b4c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteChangeSet

## Description
One net-new root cause, grounded in two failing behavioral checks that no open issue carries the replay key for.

Cause 1 of 1: the change-set-based deploy sequence the AWS CLI's `cloudformation deploy` drives (CreateChangeSet -> DescribeChangeSet polled to CREATE_COMPLETE -> ExecuteChangeSet -> DescribeStacks polled to a terminal status) never reaches a terminal outcome, so the deploy blocks and the check is killed at its budget with no AWS response to classify at all.
Failing checks: cloudformation.redeploy-with-changed-template-applies-the-change, cloudformation.redeploy-broken-importvalue-rolls-back-preserving-original
  AWS:      recorded behavior only. Both recordings run `aws cloudformation deploy --stack-name ... --template-file ... --tags ...` twice against the same stack. On AWS every call in that sequence returns: the change set reaches a terminal CREATE_COMPLETE (or FAILED with "didn't contain changes"), ExecuteChangeSet returns, and DescribeStacks reports a terminal StackStatus — CREATE_COMPLETE after the first deploy, then UPDATE_COMPLETE when the second template adds a resource, or UPDATE_ROLLBACK_COMPLETE naming the missing export when the second template references a never-declared Fn::ImportValue. The CLI exits 0 (or non-zero on the rollback), after which DescribeStackResources lists each declared resource and the added queue really round-trips a message. The replay output carries no HTTP status and no error code for these checks, because nothing was ever returned.
  Emulator: both checks produce only "Error: Check timed out after 60.0s". Some call in the deploy sequence does not return, or the change set / stack never leaves a non-terminal status, so no status, error code or message is served and the first `deploy` never gets past its wait — neither the UPDATE_COMPLETE path nor the UPDATE_ROLLBACK_COMPLETE path can be observed.
  Why:      a client that deploys with the standard CLI (or any wrapper that creates a change set, executes it and waits for the stack) blocks indefinitely instead of being told the outcome: a CI step hangs until its own timeout rather than succeeding, and a client that relies on the rollback path never learns that its template referenced a missing export, nor that the previously deployed resources were left untouched.
  Fix:      the CloudFormation service's change-set create/execute path and the stack create/update provisioning loop whose status the describe calls read — every request in the sequence must answer, and the change set and the stack must each reach a terminal status.
  Traces:   cloudformation.redeploy-with-changed-template-applies-the-change, cloudformation.redeploy-broken-importvalue-rolls-back-preserving-original. Open issue 20261114T035529Z-cloudformation-2-net-new-causes-an-out-of.md describes a comparable timeout for cloudformation.cli-script-cfn-deploy-and-verify but does not carry these two checks' replay keys, so this cause is filed here; the only diagnosis the replay gives is the timeout itself, since no response was served to classify.

## Evidence
Expected (redeploy applies the change): the first `aws cloudformation deploy` returns rc=0 and DescribeStacks reports StackStatus CREATE_COMPLETE; a second deploy of a template that adds a queue returns rc=0 and DescribeStacks reports UPDATE_COMPLETE with DescribeStackResources listing both logical ids as CREATE_COMPLETE, and the added queue sends and receives a message.
Actual (replay): Error: Check timed out after 60.0s — no HTTP status, error code or message was served.

Expected (broken ImportValue redeploy): the second deploy, whose added resource tags a never-declared export, exits non-zero and the stack rolls back to UPDATE_ROLLBACK_COMPLETE naming the missing export, with the original queue untouched and the new one never created; a third, valid deploy still succeeds.
Actual (replay): Error: Check timed out after 60.0s — the sequence never produces a terminal stack status.

## Steps to Reproduce
1. Write a template with one AWS::SQS::Queue and run `aws cloudformation deploy --stack-name <name> --template-file <v1.json> --tags sim2real:check=<id>` against the emulator. Expected: the command returns 0 and DescribeStacks(StackName=<name>) reports CREATE_COMPLETE. Observed: the command does not return and the check is killed at 60 s with no response served.
2. Add a second AWS::SQS::Queue to the template and run the same `deploy` again. Expected: rc=0 and DescribeStacks reports UPDATE_COMPLETE with both resources CREATE_COMPLETE. Observed: the same hang.
3. Instead add a resource whose tag value is {"Fn::ImportValue": "<export that was never declared>"} and run `deploy`. Expected: non-zero exit and the stack at UPDATE_ROLLBACK_COMPLETE naming the missing export, with the first queue still present and the new one absent. Observed: the same hang, so neither the rollback nor its reason is ever reported.
